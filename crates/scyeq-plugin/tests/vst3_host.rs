//! Loads the built ScyEQ-Pro VST3 library the way a host does: its parameters, its state and its sound.

#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_void};
use std::ptr;
use std::sync::{Mutex, MutexGuard, OnceLock};

use scyeq_dsp::params;
use scyeq_plugin::plugin_format::{Kind, Map};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{kCanAutomate, kIsList};
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

mod built;
mod cases;
mod vst3_module;

/// The module entry applies to the whole library, so tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn library() -> &'static libloading::Library {
    static LIBRARY: OnceLock<libloading::Library> = OnceLock::new();
    LIBRARY.get_or_init(|| vst3_module::load(&built::plugin("scyeq_plugin", "vst3")))
}

fn wide(chars: &[TChar]) -> String {
    let len = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16(&chars[..len]).unwrap()
}

struct Instance {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: ComPtr<IEditController>,
}

impl Instance {
    fn new() -> Self {
        unsafe {
            let get: libloading::Symbol<extern "system" fn() -> *mut IPluginFactory> =
                library().get(b"GetPluginFactory\0").unwrap();
            let factory = ComPtr::from_raw(get()).expect("a factory");
            assert_eq!(factory.countClasses(), 1);
            let mut info: PClassInfo = std::mem::zeroed();
            assert_eq!(factory.getClassInfo(0, &mut info), kResultOk);
            assert_eq!(&info.cid.map(|c| c as u8), b"ScytheScyEQPro01");
            assert_eq!(CStr::from_ptr(info.name.as_ptr()), c"ScyEQ-Pro");
            let mut obj = ptr::null_mut();
            assert_eq!(
                factory.createInstance(info.cid.as_ptr(), IComponent::IID.as_ptr() as FIDString, &mut obj),
                kResultOk
            );
            let component = ComPtr::from_raw(obj as *mut IComponent).expect("a component");
            assert_eq!(component.initialize(ptr::null_mut()), kResultOk);
            let processor = component.cast::<IAudioProcessor>().expect("IAudioProcessor");
            let controller = component.cast::<IEditController>().expect("IEditController");
            let mut inputs = [SpeakerArr::kStereo, SpeakerArr::kStereo];
            let mut output = SpeakerArr::kStereo;
            assert_eq!(processor.setBusArrangements(inputs.as_mut_ptr(), 2, &mut output, 1), kResultTrue);
            Self { component, processor, controller }
        }
    }

    fn plain(&self, id: u32, normalised: f64) -> f64 {
        unsafe { self.controller.normalizedParamToPlain(id, normalised) }
    }

    fn value(&self, id: u32) -> f64 {
        self.plain(id, unsafe { self.controller.getParamNormalized(id) })
    }

    fn save(&self) -> Vec<u8> {
        let saved = memory(Vec::new());
        assert_eq!(unsafe { self.component.getState(saved.to_com_ptr::<IBStream>().unwrap().as_ptr()) }, kResultOk);
        saved.bytes.borrow().clone()
    }

    fn load(&self, bytes: &[u8]) -> bool {
        let source = memory(bytes.to_vec());
        let stream = source.to_com_ptr::<IBStream>().unwrap();
        unsafe {
            let loaded = self.component.setState(stream.as_ptr()) == kResultOk;
            source.at.set(0);
            assert_eq!(self.controller.setComponentState(stream.as_ptr()), kResultOk);
            loaded
        }
    }

    fn activate(&self, rate: f64, block: i32) {
        unsafe {
            let mut setup = ProcessSetup {
                processMode: 0,
                symbolicSampleSize: kSample32 as i32,
                maxSamplesPerBlock: block,
                sampleRate: rate,
            };
            assert_eq!(self.processor.setupProcessing(&mut setup), kResultOk);
            assert_eq!(self.component.setActive(1), kResultOk);
            assert_eq!(self.processor.setProcessing(1), kResultOk);
        }
    }

    fn deactivate(&self) {
        unsafe {
            self.processor.setProcessing(0);
            assert_eq!(self.component.setActive(0), kResultOk);
        }
    }

    /// Runs `input` (both channels the same) through in blocks of `block` frames, without any
    /// parameter changes.
    fn run(&self, input: &[f32], block: usize) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len());
        for chunk in input.chunks(block) {
            let mut ins = [chunk.to_vec(), chunk.to_vec()];
            let mut outs = [vec![0.0f32; chunk.len()], vec![0.0f32; chunk.len()]];
            let mut in_ptrs = [ins[0].as_mut_ptr(), ins[1].as_mut_ptr()];
            let mut out_ptrs = [outs[0].as_mut_ptr(), outs[1].as_mut_ptr()];
            let bus = |pointers: &mut [*mut f32; 2]| AudioBusBuffers {
                numChannels: 2,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 { channelBuffers32: pointers.as_mut_ptr() },
            };
            let mut input = bus(&mut in_ptrs);
            let mut out = bus(&mut out_ptrs);
            let mut data = ProcessData {
                processMode: 0,
                symbolicSampleSize: kSample32 as i32,
                numSamples: chunk.len() as i32,
                numInputs: 1,
                numOutputs: 1,
                inputs: &mut input,
                outputs: &mut out,
                inputParameterChanges: ptr::null_mut(),
                outputParameterChanges: ptr::null_mut(),
                inputEvents: ptr::null_mut(),
                outputEvents: ptr::null_mut(),
                processContext: ptr::null_mut(),
            };
            assert_eq!(unsafe { self.processor.process(&mut data) }, kResultOk);
            assert_eq!(outs[0], outs[1]);
            output.extend_from_slice(&outs[0]);
        }
        output
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        assert_eq!(unsafe { self.component.terminate() }, kResultOk);
    }
}

/// Takes at most 5 bytes per write and hands out at most 7 per read, as a host's stream may.
struct Memory {
    bytes: RefCell<Vec<u8>>,
    at: Cell<usize>,
}

impl Class for Memory {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for Memory {
    unsafe fn read(&self, buffer: *mut c_void, size: int32, read: *mut int32) -> tresult {
        let bytes = self.bytes.borrow();
        let at = self.at.get();
        let n = (bytes.len() - at).min(size.max(0) as usize).min(7);
        unsafe {
            ptr::copy_nonoverlapping(bytes[at..].as_ptr(), buffer as *mut u8, n);
            if !read.is_null() {
                *read = n as int32;
            }
        }
        self.at.set(at + n);
        kResultOk
    }
    unsafe fn write(&self, buffer: *mut c_void, size: int32, written: *mut int32) -> tresult {
        let n = size.clamp(0, 5) as usize;
        unsafe {
            self.bytes.borrow_mut().extend_from_slice(std::slice::from_raw_parts(buffer as *const u8, n));
            if !written.is_null() {
                *written = n as int32;
            }
        }
        kResultOk
    }
    unsafe fn seek(&self, _: int64, _: int32, _: *mut int64) -> tresult {
        kNotImplemented
    }
    unsafe fn tell(&self, at: *mut int64) -> tresult {
        unsafe { *at = self.at.get() as int64 };
        kResultOk
    }
}

fn memory(bytes: Vec<u8>) -> ComWrapper<Memory> {
    ComWrapper::new(Memory { bytes: RefCell::new(bytes), at: Cell::new(0) })
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * b.abs().max(1.0)
}

#[test]
fn reports_every_parameter() {
    let _turn = turn();
    let plugin = Instance::new();
    let ids: Vec<u32> = params::ids().collect();
    assert_eq!(unsafe { plugin.controller.getParameterCount() } as usize, ids.len());
    let automatable: Vec<u32> = params::automatable().collect();
    for (index, &id) in ids.iter().enumerate() {
        let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { plugin.controller.getParameterInfo(index as i32, &mut info) }, kResultOk);
        let spec = params::spec_of(id).unwrap();
        assert_eq!(info.id, id);
        assert_eq!(wide(&info.title), params::name(id));
        assert!(close(plugin.plain(id, 0.0), spec.min) && close(plugin.plain(id, 1.0), spec.max), "{id}");
        assert!(close(plugin.plain(id, info.defaultNormalizedValue), params::default(id)), "{id}");
        assert_eq!(info.flags & kCanAutomate != 0, automatable.contains(&id), "{id}");
        assert!(close(plugin.value(id), params::default(id)), "a new instance has the defaults");
        let listed = scyeq_plugin::params::infos()[index];
        assert_eq!(info.flags & kIsList != 0, matches!(listed.kind, Kind::List { .. }), "{id}");
        let steps = match listed.kind {
            Kind::Float { .. } => 0,
            _ => (spec.max - spec.min) as i32,
        };
        assert_eq!(info.stepCount, steps, "{id}");
    }
    for id in cases::frequencies() {
        let listed = scyeq_plugin::params::infos().iter().find(|p| p.id == id).unwrap();
        assert!(matches!(listed.kind, Kind::Float { map: Map::Log, .. }), "{id}");
        let spec = params::spec_of(id).unwrap();
        assert!(close(plugin.plain(id, 0.5), (spec.min * spec.max).sqrt()), "{id} maps logarithmically");
    }
    let freq = params::band_param(3, params::band::FREQ);
    let mut text = [0 as TChar; 128];
    let normalised = params::to_normalized(freq, 2500.0);
    assert_eq!(unsafe { plugin.controller.getParamStringByValue(freq, normalised, &mut text) }, kResultOk);
    assert_eq!(wide(&text), "2.50 kHz");
}

#[test]
fn loads_scythe_state() {
    let _turn = turn();
    let plugin = Instance::new();
    assert!(plugin.load(cases::SONG2));
    assert_eq!(plugin.save(), cases::SONG2);
    let stored = params::decode(cases::SONG2).unwrap().values;
    for id in params::ids() {
        assert!(close(plugin.value(id), stored[id as usize]), "{}", params::name(id));
    }
    assert!(!plugin.load(b"not a state"));
    assert_eq!(plugin.save(), cases::SONG2, "a refused state changes nothing");
}

#[test]
fn loads_old_titled_slot() {
    let _turn = turn();
    let state = cases::old_titled_slot_state();
    let plugin = Instance::new();
    assert!(plugin.load(&state));
    assert_eq!(plugin.save(), state);
    let stored = params::decode(&state).unwrap().values;
    assert!(cases::frequencies().all(|id| close(plugin.value(id), stored[id as usize])));
}

#[test]
fn sample_rate_change_rebuilds() {
    let _turn = turn();
    let plugin = Instance::new();
    assert!(plugin.load(&cases::peak_at_1k()));
    plugin.activate(44_100.0, 512);
    plugin.run(&cases::sine(44_100.0, 1000.0, 4096), 512);
    plugin.deactivate();
    plugin.activate(96_000.0, 512);
    let input = cases::sine(96_000.0, 1000.0, 96_000);
    let output = plugin.run(&input, 512);
    let tail = input.len() - 9600;
    let gain = cases::gain_db(&input[tail..], &output[tail..]);
    assert!((gain - 6.0).abs() < 0.01, "{gain} dB at 1 kHz");
    plugin.deactivate();
}

#[test]
fn process_before_flush() {
    let _turn = turn();
    let plugin = Instance::new();
    plugin.activate(48_000.0, 256);
    let input = cases::noise(4096, 3);
    assert!(plugin.run(&input, 256) == input);
    plugin.deactivate();
}
