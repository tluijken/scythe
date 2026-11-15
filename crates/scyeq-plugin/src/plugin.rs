use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::{Arc, Mutex, Weak};

use plugin_format::{Block, GuiContext, Info, Layout, ParamInfo, Plugin, Shared, TrackInfo};
use scyeq_dsp::probe::{FxProbe, Side};
use scyeq_dsp::{Meters, ScytheEq, params};
use scyeq_ui::ViewState;

use crate::host::Editor;
use crate::registry::{self, Registration};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// What the host, the editor and the audio thread share: the settings as `f64` bits, and what
/// the state keeps besides them.
pub struct ScyEqShared {
    /// For each editor to keep the shared half it draws.
    me: Weak<ScyEqShared>,
    /// Unique among the library's instances for the life of the process; never 0.
    id: u64,
    /// The instance's, once the host is known: other instances' editors change this one's
    /// parameters through it, so its host records the change.
    cx: Mutex<Option<GuiContext>>,
    values: Box<[AtomicU64]>,
    /// Set after a value changed, for the audio thread to pick the values up.
    changed: AtomicBool,
    /// Odd while a state is being loaded into `values`; counts up once more when it is done, so
    /// the audio thread can tell it read a state whole.
    loading: AtomicU64,
    stored: Mutex<Stored>,
    track: Mutex<TrackInfo>,
    /// Width and height at a scale of 1.0, as `width << 32 | height`; 0 until resized.
    editor_size: AtomicU64,
    probe: Arc<FxProbe>,
    /// The running EQ's, from `activate` on.
    meters: Mutex<Option<Arc<Meters>>>,
    latency: AtomicU32,
    /// The host's, from `activate` on; 0 before.
    sample_rate: AtomicU32,
    /// Kept for the next editor while none is open; not part of the state.
    view: Mutex<ViewState>,
}

/// Values a newer version stored, written back unchanged, the known values stored outside their
/// range, written back while left alone, and the version that wrote them.
struct Stored {
    extra: Vec<f64>,
    unclamped: Vec<(u32, f64)>,
    version: u32,
}

impl ScyEqShared {
    fn new(me: &Weak<ScyEqShared>) -> Self {
        Self {
            me: me.clone(),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            cx: Mutex::new(None),
            values: params::defaults().into_iter().map(|v| AtomicU64::new(v.to_bits())).collect(),
            changed: AtomicBool::new(false),
            loading: AtomicU64::new(0),
            stored: Mutex::new(Stored { extra: Vec::new(), unclamped: Vec::new(), version: params::VERSION }),
            track: Mutex::new(TrackInfo::default()),
            editor_size: AtomicU64::new(0),
            probe: Arc::new(FxProbe::default()),
            meters: Mutex::new(None),
            latency: AtomicU32::new(0),
            sample_rate: AtomicU32::new(0),
            view: Mutex::new(ViewState::default()),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn cx(&self) -> Option<GuiContext> {
        self.cx.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn values(&self) -> Vec<f64> {
        self.values.iter().map(|v| f64::from_bits(v.load(Ordering::Relaxed))).collect()
    }

    /// The version of ScyEQ-Pro that wrote the loaded state.
    pub fn version(&self) -> u32 {
        self.stored.lock().unwrap_or_else(|e| e.into_inner()).version
    }

    /// Tells the user when the loaded state was written by a newer ScyEQ-Pro than this one.
    pub fn notice(&self) -> Option<String> {
        (self.version() > params::VERSION)
            .then(|| "Made with a newer ScyEQ-Pro; update to hear every setting.".to_string())
    }

    pub fn track(&self) -> TrackInfo {
        self.track.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// What the analyzer reads: the audio before and after the EQ.
    pub fn probe(&self) -> &Arc<FxProbe> {
        &self.probe
    }

    pub fn meters(&self) -> Option<Arc<Meters>> {
        self.meters.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Processing latency in samples.
    pub fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    /// The host's sample rate, or 48 kHz until the plugin is first activated.
    pub fn sample_rate(&self) -> u32 {
        match self.sample_rate.load(Ordering::Relaxed) {
            0 => 48_000,
            rate => rate,
        }
    }

    pub(crate) fn view(&self) -> ViewState {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn set_view(&self, view: ViewState) {
        *self.view.lock().unwrap_or_else(|e| e.into_inner()) = view;
    }
}

impl Shared for ScyEqShared {
    fn params(&self) -> &'static [ParamInfo] {
        crate::params::infos()
    }

    fn param(&self, id: u32) -> f64 {
        self.values.get(id as usize).map_or(0.0, |v| f64::from_bits(v.load(Ordering::Relaxed)))
    }

    fn set_param(&self, id: u32, plain: f64) {
        if params::spec_of(id).is_none() {
            return;
        }
        if let Some(slot) = self.values.get(id as usize) {
            slot.store(params::clamp(id, plain).to_bits(), Ordering::Relaxed);
            self.changed.store(true, Ordering::Release);
        }
    }

    fn param_text(&self, id: u32, plain: f64) -> String {
        params::format(id, plain)
    }

    fn param_from_text(&self, id: u32, text: &str) -> Option<f64> {
        crate::params::from_text(id, text)
    }

    fn save(&self) -> Vec<u8> {
        let stored = self.stored.lock().unwrap_or_else(|e| e.into_inner());
        params::encode(&params::unclamp(&self.values(), &stored.unclamped), &stored.extra, stored.version)
    }

    fn load(&self, bytes: &[u8]) -> bool {
        let Some(decoded) = params::decode(bytes) else { return false };
        let mut stored = self.stored.lock().unwrap_or_else(|e| e.into_inner());
        self.loading.fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        for (slot, v) in self.values.iter().zip(&decoded.values) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
        self.loading.fetch_add(1, Ordering::Release);
        *stored = Stored { extra: decoded.extra, unclamped: decoded.unclamped, version: decoded.version };
        self.changed.store(true, Ordering::Release);
        true
    }

    fn editor(&self, cx: GuiContext) -> plugin_format::Editor {
        let Some(shared) = self.me.upgrade() else { return Box::new(|_| {}) };
        let mut editor = Editor::new(shared, cx);
        Box::new(move |root| editor.frame(root))
    }

    fn attach(&self, cx: GuiContext) {
        *self.cx.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx);
    }

    fn set_track(&self, info: TrackInfo) {
        *self.track.lock().unwrap_or_else(|e| e.into_inner()) = info;
    }

    fn editor_size(&self) -> Option<(u32, u32)> {
        let size = self.editor_size.load(Ordering::Relaxed);
        (size != 0).then_some(((size >> 32) as u32, size as u32))
    }

    fn set_editor_size(&self, width: u32, height: u32) {
        self.editor_size.store((width as u64) << 32 | height as u64, Ordering::Relaxed);
    }
}

pub struct ScyEqPlugin {
    shared: Arc<ScyEqShared>,
    /// Built by `activate` from the values at the time, at the host's sample rate.
    eq: Option<Running>,
    /// Room to read the values into on the audio thread.
    scratch: Vec<f64>,
    latency: u32,
    latency_changed: bool,
    _registration: Registration,
}

struct Running {
    eq: ScytheEq,
    meters: Arc<Meters>,
    sample_rate: u32,
    inputs: usize,
    /// The second channel when the host gives only one output.
    spare: Vec<f32>,
}

impl ScyEqPlugin {
    /// Hands values changed since the last block to the EQ, as Scythe hands it a changed state.
    fn follow_values(&mut self) {
        let Some(running) = &mut self.eq else { return };
        if !self.shared.changed.swap(false, Ordering::Acquire) {
            return;
        }
        let loading = self.shared.loading.load(Ordering::Acquire);
        let mut differs = false;
        for (id, (into, value)) in self.scratch.iter_mut().zip(&self.shared.values).enumerate() {
            *into = f64::from_bits(value.load(Ordering::Relaxed));
            differs |= *into != running.eq.value(id as u32);
        }
        fence(Ordering::Acquire);
        if loading % 2 == 1 || self.shared.loading.load(Ordering::Relaxed) != loading {
            // Part of a state being loaded: the next block takes it whole.
            self.shared.changed.store(true, Ordering::Release);
            return;
        }
        if !differs {
            return;
        }
        running.eq.set_all(&self.scratch);
        let latency = running.eq.latency() as u32;
        if latency != self.latency {
            self.latency = latency;
            self.latency_changed = true;
            self.shared.latency.store(latency, Ordering::Relaxed);
        }
    }
}

impl Running {
    /// One block through the EQ, tapped before and after the way Scythe taps its effects.
    fn run(
        &mut self,
        probe: &FxProbe,
        left: &mut [f32],
        right: &mut [f32],
        aux: Option<(&[f32], &[f32])>,
        position: Option<i64>,
    ) {
        let probing = probe.active();
        if probing {
            probe.tap(Side::Pre, left, right, self.sample_rate);
        }
        self.eq.process_block(left, right, aux, position);
        if let Some((l, r)) = aux
            && self.meters.sidechain.active()
        {
            self.meters.sidechain.tap(Side::Pre, l, r, self.sample_rate);
        }
        if probing {
            probe.tap(Side::Post, left, right, self.sample_rate);
        }
    }
}

impl Plugin for ScyEqPlugin {
    const INFO: Info = Info {
        name: "ScyEQ-Pro",
        vendor: "Scythe",
        version: env!("CARGO_PKG_VERSION"),
        clap_id: "scythe.eq",
        description: "Scythe's parametric EQ: dynamic, spectral and linear phase bands, with a sidechain",
        clap_features: &[c"audio-effect", c"equalizer", c"stereo"],
        vst3_class_id: *b"ScytheScyEQPro01",
        vst3_subcategories: "Fx|EQ",
        sidechain: true,
    };
    const EDITOR_SIZE: (u32, u32) = (1180, 680);
    const RESIZABLE: bool = true;
    const EDITOR_MIN: (u32, u32) = (760, 440);
    const LAYOUTS: &'static [Layout] =
        &[Layout { name: "Stereo", main_inputs: 2 }, Layout { name: "Mono in, stereo out", main_inputs: 1 }];

    type Shared = ScyEqShared;

    fn new() -> (Self, Arc<ScyEqShared>) {
        let shared = Arc::new_cyclic(ScyEqShared::new);
        let plugin = Self {
            shared: shared.clone(),
            eq: None,
            scratch: vec![0.0; params::TOTAL],
            latency: 0,
            latency_changed: false,
            _registration: registry::register(&shared),
        };
        (plugin, shared)
    }

    fn activate(&mut self, sample_rate: f64, max_block: usize, inputs: usize) -> u32 {
        self.shared.changed.store(false, Ordering::Release);
        let sample_rate = sample_rate.round() as u32;
        let eq = ScytheEq::with_values(self.shared.values(), sample_rate);
        self.latency = eq.latency() as u32;
        self.latency_changed = false;
        let meters = eq.meters().clone();
        self.shared.latency.store(self.latency, Ordering::Relaxed);
        self.shared.sample_rate.store(sample_rate, Ordering::Relaxed);
        *self.shared.meters.lock().unwrap_or_else(|e| e.into_inner()) = Some(meters.clone());
        self.eq = Some(Running { eq, meters, sample_rate, inputs, spare: vec![0.0; max_block] });
        self.latency
    }

    fn deactivate(&mut self) {}

    fn latency(&self) -> u32 {
        self.latency
    }

    fn process(&mut self, block: Block<'_>) {
        self.follow_values();
        let Some(running) = &mut self.eq else { return };
        let Block { channels, sidechain, position, playing } = block;
        // As Scythe runs it: a stopped transport leaves the grid to carry on by itself.
        let position = position.filter(|_| playing);
        let aux = sidechain.map(|[l, r]| (l, r));
        let probe = &self.shared.probe;
        match channels {
            [left, right] => {
                if running.inputs == 1 {
                    right.copy_from_slice(left);
                }
                running.run(probe, left, right, aux, position);
            }
            [mono] => {
                let mut spare = std::mem::take(&mut running.spare);
                if !spare.is_empty() {
                    let room = spare.len();
                    for (i, chunk) in mono.chunks_mut(room).enumerate() {
                        let (at, n) = (i * room, chunk.len());
                        let right = &mut spare[..n];
                        right.copy_from_slice(chunk);
                        let aux = aux.map(|(l, r)| (&l[at..at + n], &r[at..at + n]));
                        running.run(probe, chunk, right, aux, position.map(|p| p + at as i64));
                    }
                }
                running.spare = spare;
            }
            _ => {}
        }
    }

    fn take_latency_changed(&mut self) -> bool {
        std::mem::take(&mut self.latency_changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state loaded while the audio thread picks the values up reaches the EQ whole: the
    /// values it runs with are always one state's.
    #[test]
    fn a_state_loaded_meanwhile_is_taken_whole_or_not_at_all() {
        let states: Vec<Vec<f64>> = [100.0, 5000.0]
            .into_iter()
            .map(|hz| {
                let mut v = params::defaults();
                for b in 0..params::BANDS {
                    v[params::band_param(b, params::band::FREQ) as usize] = hz;
                }
                v
            })
            .collect();
        let (mut plugin, shared) = ScyEqPlugin::new();
        plugin.activate(48_000.0, 64, 2);
        let stop = Arc::new(AtomicBool::new(false));
        let loader = {
            let (shared, stop) = (shared.clone(), stop.clone());
            let bytes: Vec<Vec<u8>> = states.iter().map(|v| params::encode(v, &[], params::VERSION)).collect();
            std::thread::spawn(move || {
                for bytes in bytes.iter().cycle() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    assert!(shared.load(bytes));
                }
            })
        };
        for _ in 0..20_000 {
            plugin.follow_values();
            let values = plugin.eq.as_ref().unwrap().eq.values();
            assert!(values == params::defaults() || states.iter().any(|s| s == values), "a mixed state");
        }
        stop.store(true, Ordering::Relaxed);
        loader.join().unwrap();
        plugin.follow_values();
        let values = plugin.eq.as_ref().unwrap().eq.values();
        assert!(states.iter().any(|s| s == values), "the last state loaded is taken");
    }
}
