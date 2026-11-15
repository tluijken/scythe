//! Instances in one host process find each other: through the library's own CLAP entry, each on
//! a track of its own, drawn with `egui_kittest`.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use clap_sys::color::clap_color;
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_host_params};
use clap_sys::ext::track_info::{
    CLAP_EXT_TRACK_INFO, CLAP_TRACK_INFO_HAS_TRACK_COLOR, CLAP_TRACK_INFO_HAS_TRACK_NAME, clap_host_track_info,
    clap_track_info,
};
use clap_sys::host::clap_host;
use clap_sys::version::CLAP_VERSION;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use in_process::{Plugin, Sent, button, clear, nothing, rescan};
use scyeq_dsp::params::{self, band};
use scyeq_plugin::plugin_format::{Editor, egui};
use scyeq_plugin::{ScyEqShared, registry};

mod in_process;

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

/// An instance on a track, with a host of its own to tell it about the track, and band 1 a
/// bell at 1 kHz. The instance goes before its host.
struct Instance {
    plugin: Plugin,
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
        Self { plugin: Plugin::new(&*host), _host: host, track }
    }

    fn harness(&self) -> Harness<'static, Editor> {
        let mut h = self.plugin.harness();
        h.run_steps(2);
        h
    }
}

impl std::ops::Deref for Instance {
    type Target = Plugin;

    fn deref(&self) -> &Plugin {
        &self.plugin
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
    assert!(h.query_by_label("Open").is_none() && h.query_by_label("Close").is_none(), "no editor to open");
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
    let mut h = kick.harness();
    open_match(&mut h);
    h.get_by_value("Choose…").click();
    h.run_steps(2);
    for label in &labels {
        assert!(h.query_by_label(label).is_some(), "{label} offered as it is named");
    }
    drop(h);
    drop(first);
    assert_eq!(found_by("Kick")[0].0, labels[1], "a number stays with its instance");
    drop((kick, second));
}

#[test]
fn a_closed_instance_leaves_its_number_to_the_next() {
    let _turn = turn();
    let (kick, first, second) = (Instance::on("Kick", [200, 40, 40]), Instance::unnamed(), Instance::unnamed());
    let labels = |list: Vec<(String, Option<[u8; 3]>)>| list.into_iter().map(|(label, _)| label).collect::<Vec<_>>();
    let before = labels(found_by("Kick"));
    drop(first);
    let third = Instance::unnamed();
    let after = labels(found_by("Kick"));
    assert_eq!(after, [before[1].clone(), before[0].clone()], "the new instance numbered as the closed one was");
    for label in &after {
        let n: usize = label.trim_start_matches("ScyEQ-Pro ").parse().unwrap();
        assert!(n <= everyone().len(), "{label} among {} instances", everyone().len());
    }
    drop((kick, second, third));
}
