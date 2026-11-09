//! ReaComp, from the parameters it stores. Every parameter follows REAPER 7.73 renders sample
//! for sample (see `examples/stock_ref.rs`), but for auto release with releases under 4 ms
//! (REAPER then floors the release at about 2.5 ms in a way not modelled) and the deprecated
//! anti-alias setting.

use super::params::{Params, Spec, Unit, spec};
use super::{DetectorFilter, DetectorInput, Rms, marked_params};
use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};

const SPECS: &[Spec] = &[
    spec(0, "Threshold", Unit::Gain, 2.0),
    spec(1, "Ratio", Unit::Ratio, 1.0),
    spec(2, "Attack (ms)", Unit::Ms(500.0), 1.0),
    spec(3, "Release (ms)", Unit::Ms(5000.0), 1.0),
    spec(4, "Pre-comp (ms)", Unit::Ms(250.0), 1.0),
    spec(6, "Detector lowpass (Hz)", Unit::Hz, 1.0),
    spec(7, "Detector highpass (Hz)", Unit::Hz, 1.0),
    spec(10, "Dry", Unit::Gain, 2.0),
    spec(11, "Wet", Unit::Gain, 2.0),
    spec(12, "Preview filter", Unit::Switch, 1.0),
    spec(13, "RMS size (ms)", Unit::Ms(100.0), 10.0),
    spec(14, "Knee (dB)", Unit::Knee, 1.0),
    spec(15, "Auto make-up", Unit::Switch, 1.0),
    spec(16, "Auto release", Unit::Switch, 1.0),
    spec(17, "Legacy attack/knee", Unit::Steps(4.0), 1.0),
];

/// A fresh ReaComp's parameters as REAPER 7.73 stores them.
const DEFAULTS: [f32; 21] = [
    1.0,
    0.030303,
    0.006,
    0.02,
    0.0,
    0.0,
    1.0,
    0.0,
    0.0,
    0.0,
    3.162_277_6e-8,
    1.0,
    0.0,
    0.05,
    0.0,
    0.0,
    0.0,
    0.25,
    0.0,
    0.0,
    0.0,
];

/// Auto release stretches the release to this many ms per dB of gain reduction (48 ms per
/// halving of the gain).
const AUTO_RELEASE_MS_PER_DB: f64 = 48.0 / 6.020_599_913_279_624;

/// The parameters as the compressor uses them.
struct Settings {
    curve: Curve,
    dry: f32,
    wet: f32,
    preview: bool,
}

impl Settings {
    fn new(p: &Params, sample_rate: u32) -> Self {
        let ratio = 1.0 + p.get(1) as f64 * 99.0;
        let legacy = p.get(17);
        let curve = CurveSpec {
            threshold: p.get(0) as f64,
            ratio: if ratio >= 99.0 { 10000.0 } else { ratio },
            knee_db: p.get(14) as f64 * 24.0,
            soft_knee: (legacy * 4.0) as i32 & 1 == 1,
            classic_attack: legacy > 0.5,
            attack_frames: p.get(2) as f64 * 500.0 * 0.001 * sample_rate as f64,
            release_ms: p.get(3) as f64 * 5000.0,
            auto_release: p.get(16) >= 0.5,
            auto_makeup: p.get(15) >= 0.5,
        };
        Self { curve: curve.build(sample_rate), dry: p.get(10), wet: p.get(11), preview: p.get(12) >= 0.5 }
    }
}

/// What a [`Curve`] is made from.
pub(crate) struct CurveSpec {
    pub threshold: f64,
    pub ratio: f64,
    pub knee_db: f64,
    pub soft_knee: bool,
    pub classic_attack: bool,
    pub attack_frames: f64,
    pub release_ms: f64,
    pub auto_release: bool,
    pub auto_makeup: bool,
}

impl CurveSpec {
    pub fn build(self, sample_rate: u32) -> Curve {
        let threshold = self.threshold.max(1e-12);
        let slope = 1.0 - 1.0 / self.ratio;
        Curve {
            threshold,
            slope,
            knee_db: self.knee_db,
            knee: 10f64.powf(self.knee_db / 20.0),
            soft_knee: self.soft_knee,
            classic_attack: self.classic_attack,
            attack: coefficient(self.attack_frames),
            release_ms: self.release_ms,
            auto_release: self.auto_release,
            // ReaXcomp ratios under 1:1 expand, and make up nothing.
            makeup: if self.auto_makeup && slope > 0.0 { 10f64.powf(-slope * threshold.log10() / 2.0) } else { 1.0 },
            sample_rate,
        }
    }
}

/// ReaComp's gain computer and envelope timing; ReaXcomp's bands use the same.
pub(crate) struct Curve {
    threshold: f64,
    /// 1 - 1/ratio.
    slope: f64,
    knee_db: f64,
    knee: f64,
    soft_knee: bool,
    classic_attack: bool,
    attack: f64,
    release_ms: f64,
    auto_release: bool,
    makeup: f64,
    sample_rate: u32,
}

fn coefficient(frames: f64) -> f64 {
    if frames <= 0.0 { 0.0 } else { (-1.0 / frames).exp() }
}

/// The envelope a [`Curve`] drives.
#[derive(Default)]
pub(crate) struct Envelope {
    env: f64,
    reduction_db: f64,
}

impl Envelope {
    /// Follows detector level `level` for one frame; returns the gain, make-up included.
    pub fn step(&mut self, c: &Curve, level: f64) -> f64 {
        if level > self.env {
            // Rising levels start from where reduction begins: below it the envelope is
            // irrelevant (unless the legacy options ask for the classic attack).
            if !c.classic_attack {
                self.env = self.env.max(c.onset());
            }
            self.env = level + c.attack * (self.env - level);
        } else {
            self.env = level + c.release(self.reduction_db) * (self.env - level);
        }
        self.reduction_db = c.reduction_db(self.env);
        10f64.powf(-self.reduction_db / 20.0) * c.makeup
    }
}

impl Curve {
    /// Gain reduction in dB for envelope level `env`.
    fn reduction_db(&self, env: f64) -> f64 {
        let x = env / self.threshold;
        let over = 20.0 * x.max(1e-30).log10();
        let k = self.knee_db;
        if k > 0.0 && over.abs() < k {
            if self.soft_knee {
                // The slope of the curve grows as x / (1 + x) through the knee.
                self.slope * 20.0 * ((1.0 + x) / (1.0 + 1.0 / self.knee)).log10()
            } else {
                self.slope * k * (x - 1.0 / self.knee) / (self.knee - 1.0 / self.knee)
            }
        } else {
            self.slope * over.max(0.0)
        }
    }

    fn release(&self, reduction_db: f64) -> f64 {
        let ms = if self.auto_release {
            self.release_ms.max(AUTO_RELEASE_MS_PER_DB * reduction_db)
        } else {
            self.release_ms
        };
        coefficient(ms * 0.001 * self.sample_rate as f64)
    }

    /// Where reduction begins: the threshold, or the bottom of the knee.
    fn onset(&self) -> f64 {
        if self.knee_db > 0.0 { self.threshold / self.knee } else { self.threshold }
    }
}

pub struct Compressor {
    params: Params,
    sample_rate: u32,
    settings: Settings,
    input: DetectorInput,
    filter: DetectorFilter,
    rms: Rms,
    window: usize,
    /// Pre-comp: the audio and the detector level are delayed by this many frames, and the
    /// envelope follows the louder of the newest and the delayed level.
    lookahead: usize,
    audio: Vec<[f32; 2]>,
    levels: Vec<f64>,
    at: usize,
    envelope: Envelope,
}

impl Compressor {
    /// Parameters 0..=20: threshold, ratio, attack, release, pre-comp, reserved, lowpass,
    /// highpass, detector input, audio input, dry, wet, filter preview, RMS size, knee, auto
    /// make-up, auto release, legacy attack/knee options, anti-alias, multichannel mode,
    /// metering index.
    pub fn from_reacomp(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let params = Params::new(marked_params(chunk)?, SPECS, &DEFAULTS);
        let mut comp = Self {
            settings: Settings::new(&params, sample_rate),
            input: DetectorInput::new(0.0),
            filter: DetectorFilter::new(1.0, 0.0, sample_rate),
            rms: Rms::new(1),
            window: 1,
            lookahead: 0,
            audio: Vec::new(),
            levels: Vec::new(),
            at: 0,
            envelope: Envelope::default(),
            params,
            sample_rate,
        };
        comp.configure(true);
        Some(comp)
    }

    /// Applies the parameters; `fresh` also rebuilds the detector's memories.
    fn configure(&mut self, fresh: bool) {
        let p = &self.params;
        let sr = self.sample_rate;
        self.settings = Settings::new(p, sr);
        self.input = DetectorInput::new(p.get(8));
        // REAPER counts whole frames of the stored f32 times.
        let frames = |raw: f32, full_ms: f64| (raw as f64 * full_ms * 0.001 * sr as f64) as usize;
        let window = frames(p.get(13), 100.0).max(1);
        let lookahead = frames(p.get(4), 250.0);
        if fresh || window != self.window {
            self.window = window;
            self.rms = Rms::new(window);
        }
        if fresh || lookahead != self.lookahead {
            self.lookahead = lookahead;
            self.audio = vec![[0.0; 2]; lookahead];
            self.levels = vec![0.0; lookahead];
            self.at = 0;
        }
        self.filter = DetectorFilter::new(p.get(6), p.get(7), sr);
    }
}

impl Effect for Compressor {
    fn params(&mut self) -> Vec<ParamInfo> {
        self.params.infos()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        self.params.display(id)
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if self.params.set(id, value) {
            self.configure(false);
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        let set = self.params.set_normalized(index, v);
        if set {
            self.configure(false);
        }
        set
    }

    fn latency(&mut self) -> usize {
        self.lookahead
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.params.chunk())
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let s = &self.settings;
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (dl, dr) = self.input.frame(i, *l, *r, ctx);
            let (fl, fr) = self.filter.run(dl as f64, dr as f64);
            let mut level = self.rms.run(fl, fr);
            let (out_l, out_r) = if self.lookahead > 0 {
                let delayed = std::mem::replace(&mut self.levels[self.at], level);
                level = level.max(delayed);
                let [al, ar] = std::mem::replace(&mut self.audio[self.at], [*l, *r]);
                self.at = (self.at + 1) % self.lookahead;
                (al, ar)
            } else {
                (*l, *r)
            };
            if s.preview {
                *l = fl as f32;
                *r = fr as f32;
                continue;
            }
            let g = self.envelope.step(&s.curve, level);
            let gain = s.dry + s.wet * g as f32;
            *l = out_l * gain;
            *r = out_r * gain;
        }
    }

    fn reset(&mut self) {
        self.envelope = Envelope::default();
        self.rms.reset();
        self.filter.reset();
        self.audio.fill([0.0; 2]);
        self.levels.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::testing::{check, chunk, run};

    #[test]
    fn compressor_matches_reaper() {
        // 4:1, attack 10 ms, release 100 ms, RMS 5 ms.
        let mut params = DEFAULTS.to_vec();
        (params[0], params[1], params[2], params[3], params[11], params[13]) = (0.2512, 0.0303, 0.02, 0.02, 1.0, 0.05);
        let gain = run(&mut Compressor::from_reacomp(&chunk(&params), 48000).unwrap());
        check(
            &gain,
            &[
                (14420, 1.0),
                (14480, 0.9954),
                (14640, 0.8777),
                (15360, 0.6404),
                (24000, 0.5968),
                (34080, 0.622),
                (36000, 0.7796),
                (38400, 0.9986),
            ],
        );
    }

    /// The static curve REAPER 7.73 renders for a level ramp: threshold -12 dB, infinite ratio
    /// (10000:1) and a 12 dB knee give 4.07 dB of reduction at the threshold, 2.41 dB with the
    /// legacy knee.
    #[test]
    fn knees_follow_reaper() {
        let mut params = DEFAULTS.to_vec();
        (params[0], params[1], params[14]) = (0.2512, 1.0, 0.5);
        let comp = Compressor::from_reacomp(&chunk(&params), 48000).unwrap();
        assert!((comp.settings.curve.reduction_db(0.2512) - 4.0735).abs() < 1e-3);
        params[17] = 0.0;
        let legacy = Compressor::from_reacomp(&chunk(&params), 48000).unwrap();
        assert!((legacy.settings.curve.reduction_db(0.2512) - 2.4089).abs() < 1e-3);
    }
}
