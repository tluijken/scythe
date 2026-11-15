//! A CLAP host for instances made through the library's own entry in the test's process, drawn
//! with `egui_kittest`: what the editor and registry tests share. Each test gives the host its
//! own extensions.

use std::ffi::{CStr, c_void};
use std::ptr;

use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_GESTURE_BEGIN, CLAP_EVENT_PARAM_GESTURE_END, CLAP_EVENT_PARAM_VALUE,
    clap_event_header, clap_event_param_gesture, clap_event_param_value, clap_input_events, clap_output_events,
};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_param_rescan_flags, clap_plugin_params};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::stream::clap_istream;
use egui::{Pos2, vec2};
use egui_kittest::Harness;
use scyeq_dsp::params::{self, band};
use scyeq_plugin::ScyEqPlugin;
use scyeq_plugin::plugin_format::{Editor, egui};

pub unsafe extern "C" fn nothing(_: *const clap_host) {}
pub unsafe extern "C" fn rescan(_: *const clap_host, _: clap_param_rescan_flags) {}
pub unsafe extern "C" fn clear(_: *const clap_host, _: u32, _: u32) {}

/// What the plugin sent, as the host's output event list received it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sent {
    Begin(u32),
    Value(u32, f64),
    End(u32),
}

/// An initialised instance, made through the library's CLAP entry for `host`, with band 1 a
/// bell at 1 kHz; destroyed when dropped.
pub struct Plugin(pub *const clap_plugin);

impl Plugin {
    /// `host` must outlive the instance.
    pub fn new(host: *const clap_host) -> Self {
        let plugin = unsafe {
            let entry = &scyeq_plugin::clap_entry;
            assert!((entry.init.unwrap())(c"".as_ptr()));
            let factory = (entry.get_factory.unwrap())(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory;
            let desc = ((*factory).get_plugin_descriptor.unwrap())(factory, 0);
            let plugin = ((*factory).create_plugin.unwrap())(factory, host, (*desc).id);
            assert!(!plugin.is_null());
            assert!(((*plugin).init.unwrap())(plugin));
            Self(plugin)
        };
        let mut values = params::defaults();
        values[params::band_param(0, band::USED) as usize] = 1.0;
        values[params::band_param(0, band::FREQ) as usize] = 1000.0;
        values[params::band_param(0, band::GAIN) as usize] = 0.0;
        plugin.load(&params::encode(&values, &[], params::VERSION));
        plugin
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

    pub fn value(&self, id: u32) -> f64 {
        let params: &clap_plugin_params = self.extension(CLAP_EXT_PARAMS);
        let mut value = f64::NAN;
        assert!(unsafe { (params.get_value.unwrap())(self.0, id, &mut value) });
        value
    }

    /// What the instance sent its host since the last flush.
    pub fn flush(&self) -> Vec<Sent> {
        let params: &clap_plugin_params = self.extension(CLAP_EXT_PARAMS);
        let mut sent = Vec::new();
        let input = clap_input_events { ctx: ptr::null_mut(), size: Some(no_events), get: Some(no_event) };
        let output = clap_output_events { ctx: &mut sent as *mut Vec<Sent> as *mut c_void, try_push: Some(record) };
        unsafe { (params.flush.unwrap())(self.0, &input, &output) };
        sent
    }

    pub fn harness(&self) -> Harness<'static, Editor> {
        let editor =
            unsafe { scyeq_plugin::plugin_format::clap::editor::<ScyEqPlugin>(self.0) }.expect("the instance's editor");
        Harness::builder()
            .with_size(vec2(1180.0, 680.0))
            .with_step_dt(1.0 / 30.0)
            .build_ui_state(|ui, editor: &mut Editor| editor(ui), editor)
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        unsafe { ((*self.0).destroy.unwrap())(self.0) };
    }
}

pub fn button(h: &mut Harness<Editor>, pos: Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
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
