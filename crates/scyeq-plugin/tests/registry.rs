//! Instances in one host process find each other: through the library's own CLAP entry, each on
//! a track of its own, drawn with `egui_kittest`.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use clap_sys::color::clap_color;
use clap_sys::events::{
    CLAP_EVENT_PARAM_GESTURE_BEGIN, CLAP_EVENT_PARAM_GESTURE_END, CLAP_EVENT_PARAM_VALUE, clap_event_header,
    clap_event_param_gesture, clap_event_param_value, clap_input_events, clap_output_events,
};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_host_params, clap_param_rescan_flags, clap_plugin_params};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::ext::track_info::{
    CLAP_EXT_TRACK_INFO, CLAP_TRACK_INFO_HAS_TRACK_COLOR, CLAP_TRACK_INFO_HAS_TRACK_NAME, clap_host_track_info,
    clap_track_info,
};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::stream::clap_istream;
use clap_sys::version::CLAP_VERSION;
use egui::{Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use scyeq_dsp::params::{self, band};
use scyeq_plugin::plugin_format::{Editor, egui};
use scyeq_plugin::{ScyEqPlugin, ScyEqShared, registry};

/// The registry is the whole library's, so tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the host says about the track an instance is on, and how often the instance asked it
/// to flush; no track info at all without a name.
struct Track {
    name: Option<&'static str>,
    color: [u8; 3],
    flushes: AtomicU32,
}

unsafe extern "C" fn nothing(_: *const clap_host) {}
unsafe extern "C" fn rescan(_: *const clap_host, _: clap_param_rescan_flags) {}
unsafe extern "C" fn clear(_: *const clap_host, _: u32, _: u32) {}

static HOST_PARAMS: clap_host_params =
    clap_host_params { rescan: Some(rescan), clear: Some(clear), request_flush: Some(request_flush) };
static HOST_TRACK_INFO: clap_host_track_info = clap_host_track_info { get: Some(track_info) };

unsafe extern "C" fn request_flush(host: *const clap_host) {
    unsafe { (*((*host).host_data as *const Track)).flushes.fetch_add(1, Ordering::SeqCst) };
}

unsafe extern "C" fn track_info(host: *const clap_host, info: *mut clap_track_info) -> bool {
    unsafe {
        let track = &*((*host).host_data as *const Track);
        let Some(name) = track.name else { return false };
        let info = &mut *info;
        info.flags = CLAP_TRACK_INFO_HAS_TRACK_NAME | CLAP_TRACK_INFO_HAS_TRACK_COLOR;
        for (at, &b) in info.name.iter_mut().zip(name.as_bytes()) {
            *at = b as c_char;
        }
        let [red, green, blue] = track.color;
        info.color = clap_color { alpha: 255, red, green, blue };
        true
    }
}

unsafe extern "C" fn host_extension(_: *const clap_host, id: *const c_char) -> *const c_void {
    match unsafe { CStr::from_ptr(id) } {
        id if id == CLAP_EXT_PARAMS => &HOST_PARAMS as *const _ as *const c_void,
        id if id == CLAP_EXT_TRACK_INFO => &HOST_TRACK_INFO as *const _ as *const c_void,
        _ => ptr::null(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Sent {
    Begin(u32),
    Value(u32, f64),
    End(u32),
}

/// An initialised instance on a track, made through the library's CLAP entry, with band 1 a
/// bell at 1 kHz; destroyed when dropped.
struct Instance {
    plugin: *const clap_plugin,
    _host: Box<clap_host>,
    track: Box<Track>,
}

impl Instance {
    fn on(name: &'static str, color: [u8; 3]) -> Self {
        Self::with(Track { name: Some(name), color, flushes: AtomicU32::new(0) })
    }

    /// On a track the host says nothing about.
    fn unnamed() -> Self {
        Self::with(Track { name: None, color: [0; 3], flushes: AtomicU32::new(0) })
    }

    fn with(track: Track) -> Self {
        let track = Box::new(track);
        let host = Box::new(clap_host {
            clap_version: CLAP_VERSION,
            host_data: &*track as *const Track as *mut c_void,
            name: c"scyeq-plugin registry test host".as_ptr(),
            vendor: c"".as_ptr(),
            url: c"".as_ptr(),
            version: c"0".as_ptr(),
            get_extension: Some(host_extension),
            request_restart: Some(nothing),
            request_process: Some(nothing),
            request_callback: Some(nothing),
        });
        let plugin = unsafe {
            let entry = &scyeq_plugin::clap_entry;
            assert!((entry.init.unwrap())(c"".as_ptr()));
            let factory = (entry.get_factory.unwrap())(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory;
            let desc = ((*factory).get_plugin_descriptor.unwrap())(factory, 0);
            let plugin = ((*factory).create_plugin.unwrap())(factory, &*host, (*desc).id);
            assert!(!plugin.is_null());
            assert!(((*plugin).init.unwrap())(plugin));
            plugin
        };
        let instance = Self { plugin, _host: host, track };
        let mut values = params::defaults();
        values[params::band_param(0, band::USED) as usize] = 1.0;
        values[params::band_param(0, band::FREQ) as usize] = 1000.0;
        values[params::band_param(0, band::GAIN) as usize] = 0.0;
        instance.load(&params::encode(&values, &[], params::VERSION));
        instance
    }

    fn extension<T>(&self, id: &CStr) -> &T {
        unsafe {
            let ext = ((*self.plugin).get_extension.unwrap())(self.plugin, id.as_ptr()) as *const T;
            assert!(!ext.is_null(), "{id:?} missing");
            &*ext
        }
    }

    fn load(&self, bytes: &[u8]) {
        let state: &clap_plugin_state = self.extension(CLAP_EXT_STATE);
        let mut source = (bytes, 0usize);
        let stream = clap_istream { ctx: &mut source as *mut (&[u8], usize) as *mut c_void, read: Some(read) };
        assert!(unsafe { (state.load.unwrap())(self.plugin, &stream) });
    }

    fn value(&self, id: u32) -> f64 {
        let params: &clap_plugin_params = self.extension(CLAP_EXT_PARAMS);
        let mut value = f64::NAN;
        assert!(unsafe { (params.get_value.unwrap())(self.plugin, id, &mut value) });
        value
    }

    /// What the instance sent its host since the last flush.
    fn flush(&self) -> Vec<Sent> {
        let params: &clap_plugin_params = self.extension(CLAP_EXT_PARAMS);
        let mut sent = Vec::new();
        let input = clap_input_events { ctx: ptr::null_mut(), size: Some(no_events), get: Some(no_event) };
        let output = clap_output_events { ctx: &mut sent as *mut Vec<Sent> as *mut c_void, try_push: Some(record) };
        unsafe { (params.flush.unwrap())(self.plugin, &input, &output) };
        sent
    }

    fn harness(&self) -> Harness<'static, Editor> {
        let editor = unsafe { scyeq_plugin::plugin_format::clap::editor::<ScyEqPlugin>(self.plugin) }
            .expect("the instance's editor");
        let mut h = Harness::builder()
            .with_size(vec2(1180.0, 680.0))
            .with_step_dt(1.0 / 30.0)
            .build_ui_state(|ui, editor: &mut Editor| editor(ui), editor);
        h.run_steps(2);
        h
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { ((*self.plugin).destroy.unwrap())(self.plugin) };
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

fn band() -> (Instance, Instance, Instance) {
    (Instance::on("Kick", [200, 40, 40]), Instance::on("Bass", [40, 200, 40]), Instance::on("Vox", [40, 40, 200]))
}

/// Every live instance; ids start at 1.
fn everyone() -> Vec<Arc<ScyEqShared>> {
    registry::others(0)
}

fn on_track(name: &str) -> Arc<ScyEqShared> {
    everyone().into_iter().find(|s| s.track().name.as_deref() == Some(name)).expect(name)
}

/// The labels and colours of the instances `name` finds, as its editor lists them.
fn found_by(name: &str) -> Vec<(String, Option<[u8; 3]>)> {
    registry::listed_others(on_track(name).id()).into_iter().map(|i| (i.label, i.color)).collect()
}

/// The EQ Match references the editor offers.
fn match_references(h: &mut Harness<Editor>) -> Vec<String> {
    h.get_by_value("Choose…").click();
    h.run_steps(2);
    let found = ["Kick", "Bass", "Vox"]
        .into_iter()
        .map(|name| format!("{name} (ScyEQ-Pro)"))
        .filter(|label| h.query_by_label(label).is_some())
        .collect();
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    found
}

fn open_match(h: &mut Harness<Editor>) {
    h.get_by_label("Match").click();
    h.run_steps(2);
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

#[test]
fn instances_see_each_other() {
    let _turn = turn();
    let (kick, _bass, _vox) = band();
    let (red, green, blue) = ([200, 40, 40], [40, 200, 40], [40, 40, 200]);
    let named = |list: &[(&str, [u8; 3])]| list.iter().map(|(n, c)| (n.to_string(), Some(*c))).collect::<Vec<_>>();
    assert_eq!(found_by("Kick"), named(&[("Bass", green), ("Vox", blue)]));
    assert_eq!(found_by("Bass"), named(&[("Kick", red), ("Vox", blue)]));
    assert_eq!(found_by("Vox"), named(&[("Kick", red), ("Bass", green)]));

    let mut h = kick.harness();
    open_match(&mut h);
    assert_eq!(match_references(&mut h), ["Bass (ScyEQ-Pro)", "Vox (ScyEQ-Pro)"]);
    h.get_by_label("Instances").click();
    h.run_steps(2);
    for name in ["Kick", "Bass", "Vox"] {
        assert!(h.query_by_label(&format!("{name} band 1")).is_some(), "{name} in the instance list");
    }
    assert!(h.query_by_label("Bypassed").is_none() && h.query_by_label("On").is_none(), "no bypass to offer");
}

#[test]
fn dropped_instance_disappears() {
    let _turn = turn();
    let (kick, bass, _vox) = band();
    let mut h = kick.harness();
    open_match(&mut h);
    h.get_by_value("Choose…").click();
    h.run_steps(2);
    h.get_by_label("Bass (ScyEQ-Pro)").click();
    h.run_steps(2);
    assert!(h.query_by_value("Bass (ScyEQ-Pro)").is_some(), "Bass is the reference");

    drop(bass);
    h.run_steps(2);
    assert!(h.query_by_value("Bass (ScyEQ-Pro)").is_none());
    assert_eq!(match_references(&mut h), ["Vox (ScyEQ-Pro)"]);
    assert_eq!(found_by("Kick").len(), 1);
}

#[test]
fn editing_another_instance_reaches_its_host() {
    let _turn = turn();
    let (kick, bass, vox) = band();
    let gain = params::band_param(0, band::GAIN);
    let mut h = kick.harness();
    h.get_by_label("Instances").click();
    h.run_steps(2);
    let from = h.get_by_label("Bass band 1").rect().center();
    let to = from + vec2(0.0, -40.0);
    h.event(egui::Event::PointerMoved(from));
    h.step();
    button(&mut h, from, true);
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
    button(&mut h, to, false);
    h.step();

    let sent = bass.flush();
    assert_eq!(sent.first(), Some(&Sent::Begin(gain)), "{sent:?}");
    assert_eq!(sent.last(), Some(&Sent::End(gain)), "{sent:?}");
    assert!(sent.iter().all(|s| matches!(s, Sent::Value(id, _) | Sent::Begin(id) | Sent::End(id) if *id == gain)));
    let Some(&Sent::Value(_, last)) = sent.iter().rev().find(|s| matches!(s, Sent::Value(..))) else {
        panic!("no value sent: {sent:?}")
    };
    assert!(last > 2.0, "dragging up raises the gain: {last}");
    assert_eq!(bass.value(gain), last, "Bass's host reads what it was sent");
    assert!(kick.flush().is_empty() && vox.flush().is_empty(), "only Bass changed");
    let flushes = |i: &Instance| i.track.flushes.load(Ordering::SeqCst);
    assert!(flushes(&bass) > 0, "Bass's host was asked to flush");
    assert_eq!((flushes(&kick), flushes(&vox)), (0, 0), "no other host was");
}

#[test]
fn closing_mid_drag_ends_the_gesture_on_the_other_instance() {
    let _turn = turn();
    let (kick, bass, _vox) = band();
    let gain = params::band_param(0, band::GAIN);
    let mut h = kick.harness();
    h.get_by_label("Instances").click();
    h.run_steps(2);
    let from = h.get_by_label("Bass band 1").rect().center();
    h.event(egui::Event::PointerMoved(from));
    h.step();
    button(&mut h, from, true);
    h.event(egui::Event::PointerMoved(from + vec2(0.0, -20.0)));
    h.step();
    drop(h);

    let sent = bass.flush();
    assert_eq!(sent.first(), Some(&Sent::Begin(gain)), "{sent:?}");
    assert_eq!(sent.last(), Some(&Sent::End(gain)), "{sent:?}");
}

#[test]
fn instances_on_unnamed_tracks_are_numbered() {
    let _turn = turn();
    let (kick, first, second) = (Instance::on("Kick", [200, 40, 40]), Instance::unnamed(), Instance::unnamed());
    let listed = found_by("Kick");
    let labels: Vec<&str> = listed.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(labels.len(), 2);
    assert!(labels.iter().all(|l| l.starts_with("ScyEQ-Pro ")) && labels[0] != labels[1], "{labels:?}");
    assert!(listed.iter().all(|(_, color)| color.is_none()));
    drop(first);
    assert_eq!(found_by("Kick")[0].0, labels[1], "a number stays with its instance");
    drop((kick, second));
}
