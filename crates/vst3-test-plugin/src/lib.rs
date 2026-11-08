//! Minimal VST 3 plugins used to test scythe's VST 3 host. "Test Gain" scales stereo audio by
//! its one parameter, keeps that gain in its component state and has a separate controller.
//! "Test Synth" is an instrument that writes the held key / 128 as a constant level.

#![allow(non_snake_case)]

use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::slice;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper, uid};

fn copy_c(src: &str, dst: &mut [c_char]) {
    for (d, s) in dst.iter_mut().zip(src.bytes().chain(std::iter::once(0))) {
        *d = s as c_char;
    }
}

fn copy_wide(src: &str, dst: &mut [TChar]) {
    for (d, s) in dst.iter_mut().zip(src.encode_utf16().chain(std::iter::once(0))) {
        *d = s as TChar;
    }
}

const GAIN_CID: TUID = uid(0x5C7E4E01, 0x2A6B4D5A, 0x9F3B1C2D, 0x3E4F5061);
const CONTROLLER_CID: TUID = uid(0x5C7E4E02, 0x2A6B4D5A, 0x9F3B1C2D, 0x3E4F5061);
const SYNTH_CID: TUID = uid(0x5C7E4E03, 0x2A6B4D5A, 0x9F3B1C2D, 0x3E4F5061);

unsafe fn read_f64(stream: *mut IBStream) -> Option<f64> {
    // SAFETY: the host passes a readable stream.
    let stream = unsafe { ComRef::from_raw(stream) }?;
    let mut bytes = [0u8; 8];
    let mut read = 0;
    unsafe { stream.read(bytes.as_mut_ptr() as *mut c_void, 8, &mut read) };
    (read == 8).then(|| f64::from_le_bytes(bytes))
}

/// Shared by the gain processor and the synth: one stereo bus or none, and an event bus.
struct Processor {
    gain: Cell<f64>,
    instrument: bool,
    key: Cell<Option<i16>>,
}

impl Class for Processor {
    type Interfaces = (IComponent, IAudioProcessor);
}

impl IPluginBaseTrait for Processor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Processor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        if self.instrument {
            return kNotImplemented;
        }
        unsafe { *class_id = CONTROLLER_CID };
        kResultOk
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media: MediaType, dir: BusDirection) -> int32 {
        match (media as MediaTypes, dir as BusDirections) {
            (MediaTypes_::kAudio, BusDirections_::kInput) => (!self.instrument) as int32,
            (MediaTypes_::kAudio, BusDirections_::kOutput) => 1,
            (MediaTypes_::kEvent, BusDirections_::kInput) => self.instrument as int32,
            _ => 0,
        }
    }
    unsafe fn getBusInfo(&self, media: MediaType, dir: BusDirection, index: int32, bus: *mut BusInfo) -> tresult {
        if index != 0 || unsafe { self.getBusCount(media, dir) } == 0 {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media;
        bus.direction = dir;
        bus.channelCount = if media == MediaTypes_::kAudio as MediaType { 2 } else { 1 };
        copy_wide("Main", &mut bus.name);
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as uint32;
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: int32, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        match unsafe { read_f64(state) } {
            Some(g) => {
                self.gain.set(g);
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else { return kResultFalse };
        let mut bytes = self.gain.get().to_le_bytes();
        let mut written = 0;
        unsafe { stream.write(bytes.as_mut_ptr() as *mut c_void, 8, &mut written) };
        kResultOk
    }
}

impl IAudioProcessorTrait for Processor {
    unsafe fn setBusArrangements(
        &self,
        _i: *mut SpeakerArrangement,
        _ni: int32,
        _o: *mut SpeakerArrangement,
        _no: int32,
    ) -> tresult {
        kResultTrue
    }
    unsafe fn getBusArrangement(&self, _dir: BusDirection, _index: int32, arr: *mut SpeakerArrangement) -> tresult {
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: int32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as int32 { kResultOk } else { kNotImplemented }
    }
    unsafe fn getLatencySamples(&self) -> uint32 {
        0
    }
    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        // SAFETY: the host passes valid process data for `numSamples` frames.
        unsafe {
            let data = &*data;
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                for i in 0..changes.getParameterCount() {
                    if let Some(queue) = ComRef::from_raw(changes.getParameterData(i))
                        && queue.getParameterId() == 0
                    {
                        let (mut offset, mut value) = (0, 0.0);
                        if queue.getPoint(queue.getPointCount() - 1, &mut offset, &mut value) == kResultTrue {
                            self.gain.set(value);
                        }
                    }
                }
            }
            let n = data.numSamples as usize;
            let out = slice::from_raw_parts(data.outputs, data.numOutputs as usize);
            let Some(out) = out.first() else { return kResultOk };
            let outs = slice::from_raw_parts(out.__field0.channelBuffers32, 2);
            if self.instrument {
                let events = ComRef::from_raw(data.inputEvents);
                let count = events.as_ref().map_or(0, |e| e.getEventCount());
                let mut next = 0;
                for i in 0..n {
                    while next < count {
                        let mut e: Event = std::mem::zeroed();
                        events.as_ref().unwrap().getEvent(next, &mut e);
                        if e.sampleOffset as usize > i {
                            break;
                        }
                        if e.r#type == Event_::EventTypes_::kNoteOnEvent as u16 {
                            self.key.set(Some(e.__field0.noteOn.pitch));
                        } else if e.r#type == Event_::EventTypes_::kNoteOffEvent as u16
                            && self.key.get() == Some(e.__field0.noteOff.pitch)
                        {
                            self.key.set(None);
                        }
                        next += 1;
                    }
                    let v = self.key.get().map_or(0.0, |k| k as f32 / 128.0);
                    *outs[0].add(i) = v;
                    *outs[1].add(i) = v;
                }
            } else {
                let input = &*data.inputs;
                let ins = slice::from_raw_parts(input.__field0.channelBuffers32, 2);
                let g = self.gain.get() as f32;
                for c in 0..2 {
                    for i in 0..n {
                        *outs[c].add(i) = *ins[c].add(i) * g;
                    }
                }
            }
        }
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> uint32 {
        0
    }
}

struct Controller {
    gain: Cell<f64>,
}

impl Class for Controller {
    type Interfaces = (IEditController,);
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        match unsafe { read_f64(state) } {
            Some(g) => {
                self.gain.set(g);
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> int32 {
        1
    }
    unsafe fn getParameterInfo(&self, index: int32, info: *mut ParameterInfo) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.id = 0;
        copy_wide("Gain", &mut info.title);
        copy_wide("Gain", &mut info.shortTitle);
        copy_wide("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = 0.5;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as int32;
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, _id: ParamID, value: ParamValue, string: *mut String128) -> tresult {
        copy_wide(&format!("{value:.2}"), unsafe { &mut *string });
        kResultOk
    }
    unsafe fn getParamValueByString(&self, _id: ParamID, _s: *mut TChar, _v: *mut ParamValue) -> tresult {
        kNotImplemented
    }
    unsafe fn normalizedParamToPlain(&self, _id: ParamID, v: ParamValue) -> ParamValue {
        v
    }
    unsafe fn plainParamToNormalized(&self, _id: ParamID, v: ParamValue) -> ParamValue {
        v
    }
    unsafe fn getParamNormalized(&self, _id: ParamID) -> ParamValue {
        self.gain.get()
    }
    unsafe fn setParamNormalized(&self, _id: ParamID, value: ParamValue) -> tresult {
        self.gain.set(value);
        kResultOk
    }
    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }
    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        std::ptr::null_mut()
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory, IPluginFactory2);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy_c("Scythe", &mut info.vendor);
        copy_c("", &mut info.url);
        copy_c("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }
    unsafe fn countClasses(&self) -> int32 {
        3
    }
    unsafe fn getClassInfo(&self, index: int32, info: *mut PClassInfo) -> tresult {
        let (cid, category, name) = match index {
            0 => (GAIN_CID, "Audio Module Class", "Test Gain"),
            1 => (CONTROLLER_CID, "Component Controller Class", "Test Gain"),
            2 => (SYNTH_CID, "Audio Module Class", "Test Synth"),
            _ => return kInvalidArgument,
        };
        let info = unsafe { &mut *info };
        info.cid = cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_c(category, &mut info.category);
        copy_c(name, &mut info.name);
        kResultOk
    }
    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        let instance = match unsafe { *(cid as *const TUID) } {
            GAIN_CID => ComWrapper::new(Processor { gain: Cell::new(0.5), instrument: false, key: Cell::new(None) })
                .to_com_ptr::<FUnknown>(),
            SYNTH_CID => ComWrapper::new(Processor { gain: Cell::new(1.0), instrument: true, key: Cell::new(None) })
                .to_com_ptr::<FUnknown>(),
            CONTROLLER_CID => ComWrapper::new(Controller { gain: Cell::new(0.5) }).to_com_ptr::<FUnknown>(),
            _ => None,
        };
        match instance {
            Some(i) => unsafe { ((*(*i.as_ptr()).vtbl).queryInterface)(i.as_ptr(), iid as *mut TUID, obj) },
            None => kInvalidArgument,
        }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: int32, info: *mut PClassInfo2) -> tresult {
        let mut base: PClassInfo = unsafe { std::mem::zeroed() };
        if unsafe { self.getClassInfo(index, &mut base) } != kResultOk {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.cid = base.cid;
        info.cardinality = base.cardinality;
        info.category = base.category;
        info.name = base.name;
        copy_c("Scythe", &mut info.vendor);
        copy_c(if index == 2 { "Instrument|Synth" } else { "Fx" }, &mut info.subCategories);
        copy_c("1.0", &mut info.version);
        copy_c("VST 3.7", &mut info.sdkVersion);
        kResultOk
    }
}

#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_library: *mut c_void) -> bool {
    true
}

#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
    true
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory).to_com_ptr::<IPluginFactory>().map_or(std::ptr::null_mut(), |p| p.into_raw())
}
