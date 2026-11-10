//! A track's fader stage as the render graph plays it: volume, polarity, the pan mode's gain
//! matrix, the levels of VCA leads, and the envelopes that move them.
//!
//! VCA leads (REAPER 7.73 renders): a follower's level is multiplied by each lead's fader,
//! volume, trim and mute envelopes, its pan is offset by the leads' pans and pan envelopes,
//! and its width is multiplied by theirs. A lead's mute button, solo and polarity leave the
//! followers alone. Following pre-FX, the leads' levels act on the track's input instead of
//! its fader, so pre-FX sends hear them too. When playback starts, a follower's pan and width
//! glide from its own to the leads' sum over the first 512 frames ([`VcaRamp`]).

use scythe_core::envelope::PointShape;
use scythe_core::fader::{PanMode, Panner};
use scythe_core::{Envelope, EnvelopeKind, Project, Track};

use crate::render::mute_gain;

/// How long a mute envelope takes to switch (see `mute_gain`).
const MUTE_FADE: f64 = 0.004;

/// A gain matrix `[l<-l, l<-r, r<-l, r<-r]`.
pub type Matrix = [f32; 4];

fn active(t: &Track, kind: EnvelopeKind) -> Option<Envelope> {
    t.envelope(kind).filter(|e| e.active && !e.points.is_empty()).map(|e| Envelope { raw: None, ..e.clone() })
}

const GAIN_KINDS: [EnvelopeKind; 3] = [EnvelopeKind::Volume, EnvelopeKind::VolumePreFx, EnvelopeKind::Trim];

/// The fader stage of one track.
#[derive(Debug, Clone)]
pub struct FaderStage {
    /// Gain for channels 3 and up, polarity included: REAPER pans only the first pair.
    pub volume: f64,
    pub matrix: Matrix,
    pub automation: Option<Automation>,
    /// Levels of VCA leads followed pre-FX, applied to the track's input.
    pub pre_fx: Option<PreFxGain>,
    pub vca_ramp: Option<VcaRamp>,
}

/// The glide a VCA follower's pan makes when playback starts: REAPER 7.73 starts the track at
/// its own pan, width and dual pan and moves those linearly to the values with its leads'
/// added over the first [`VcaRamp::FRAMES`] frames, the gains following per sample.
#[derive(Debug, Clone, Copy)]
pub struct VcaRamp {
    pub from: Panner,
    pub to: Panner,
    /// The fader with the leads' faders and the polarity sign.
    pub volume: f64,
}

impl VcaRamp {
    pub const FRAMES: i64 = 512;

    /// The gains `frames` into playback.
    pub fn matrix_at(&self, frames: i64) -> Matrix {
        let k = (frames as f64 / Self::FRAMES as f64).clamp(0.0, 1.0);
        let mix = |a: f64, b: f64| a + (b - a) * k;
        let mut p = self.to;
        p.pan = mix(self.from.pan, self.to.pan);
        p.width = mix(self.from.width, self.to.width);
        p.dual = [mix(self.from.dual[0], self.to.dual[0]), mix(self.from.dual[1], self.to.dual[1])];
        scaled(p.matrix(), self.volume)
    }
}

/// Active track envelopes and what they modulate. Gains are evaluated every
/// `Automation::STEP` frames and ramped in between.
#[derive(Debug, Clone)]
pub struct Automation {
    /// The fader with the VCA leads' faders and the polarity sign.
    pub volume: f64,
    pub panner: Panner,
    /// Envelopes whose values multiply the volume (volume, pre-FX volume, trim).
    pub gains: Vec<Envelope>,
    pub mutes: Vec<Envelope>,
    /// Envelopes added to the pan (stored with the sign flipped), the width (multiplied), and
    /// the dual pan positions (added, sign flipped).
    pub pans: Vec<Envelope>,
    pub widths: Vec<Envelope>,
    pub dual: [Vec<Envelope>; 2],
}

/// The level of VCA leads a track follows pre-FX.
#[derive(Debug, Clone)]
pub struct PreFxGain {
    pub gain: f64,
    pub gains: Vec<Envelope>,
    pub mutes: Vec<Envelope>,
}

impl PreFxGain {
    pub fn at(&self, t: f64) -> f32 {
        let mute = self.mutes.iter().map(|e| mute_gain(e, t)).product::<f64>();
        self.gains.iter().fold(self.gain * mute, |g, e| g * e.value_at(t).unwrap_or(1.0)) as f32
    }
}

impl FaderStage {
    pub fn new(project: &Project, ti: usize) -> Self {
        let t = &project.tracks[ti];
        let sign = if t.phase { -1.0 } else { 1.0 };
        let mut panner = project.panner_for(t);
        let own = panner;
        let mut volume = t.volume;
        let mut gains: Vec<Envelope> = GAIN_KINDS.into_iter().filter_map(|k| active(t, k)).collect();
        let mut mutes: Vec<Envelope> = active(t, EnvelopeKind::Mute).into_iter().collect();
        let mut pans: Vec<Envelope> = active(t, EnvelopeKind::Pan).into_iter().collect();
        let mut widths: Vec<Envelope> = active(t, EnvelopeKind::Width).into_iter().collect();
        let dual = [active(t, EnvelopeKind::DualPanLeft), active(t, EnvelopeKind::DualPanRight)].map(Vec::from_iter);
        let mut pre: Option<PreFxGain> = None;
        for (li, pre_fx) in project.vca_leads(ti) {
            let lead = &project.tracks[li];
            panner.pan += lead.pan;
            panner.width *= lead.width;
            pans.extend(active(lead, EnvelopeKind::Pan));
            widths.extend(active(lead, EnvelopeKind::Width));
            let lead_gains = GAIN_KINDS.into_iter().filter_map(|k| active(lead, k));
            let lead_mute = active(lead, EnvelopeKind::Mute);
            if pre_fx {
                let p = pre.get_or_insert_with(|| PreFxGain { gain: 1.0, gains: Vec::new(), mutes: Vec::new() });
                p.gain *= lead.volume;
                p.gains.extend(lead_gains);
                p.mutes.extend(lead_mute);
            } else {
                volume *= lead.volume;
                gains.extend(lead_gains);
                mutes.extend(lead_mute);
            }
        }
        panner.pan = panner.pan.clamp(-1.0, 1.0);
        let volume = sign * volume;
        let matrix = scaled(panner.matrix(), volume);
        let envelopes = !gains.is_empty() || !mutes.is_empty() || !pans.is_empty() || !widths.is_empty();
        let automation = (envelopes || dual.iter().any(|d| !d.is_empty()) && panner.mode == PanMode::Dual)
            .then_some(Automation { volume, panner, gains, mutes, pans, widths, dual });
        let vca_ramp = (automation.is_none() && own.matrix() != panner.matrix()).then_some(VcaRamp {
            from: own,
            to: panner,
            volume,
        });
        Self { volume, matrix, automation, pre_fx: pre, vca_ramp }
    }
}

fn scaled(m: [f64; 4], g: f64) -> Matrix {
    m.map(|v| (v * g) as f32)
}

/// Whether a matrix only scales each channel.
pub fn is_diagonal(m: &Matrix) -> bool {
    m[1] == 0.0 && m[2] == 0.0
}

/// Applies `m` to a pair in place.
pub fn apply(m: &Matrix, l: &mut f32, r: &mut f32) {
    let (a, b) = (*l, *r);
    *l = m[0] * a + m[1] * b;
    *r = m[2] * a + m[3] * b;
}

pub fn lerp(a: &Matrix, b: &Matrix, k: f32) -> Matrix {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * k)
}

impl Automation {
    pub const STEP: i64 = 32;
    /// REAPER evaluates pan envelopes once per 512-frame processing block and ramps the channel
    /// gains linearly in between; a block cut short by the end of a render ramps to its end.
    const PAN_GRID: i64 = 512;

    /// Mute envelopes crossfade over 4 ms like a send's (see `mute_gain`).
    fn mute(&self, t: f64) -> f64 {
        self.mutes.iter().map(|e| mute_gain(e, t)).product()
    }

    /// The volume part alone (fader, volume envelopes, mute), for channels 3 and up.
    pub fn volume_at(&self, t: f64) -> f32 {
        self.gains.iter().fold(self.volume * self.mute(t), |v, e| v * e.value_at(t).unwrap_or(1.0)) as f32
    }

    fn pan_like(&self) -> impl Iterator<Item = &Envelope> {
        let dual = self.dual.iter().flatten().filter(|_| self.panner.mode == PanMode::Dual);
        let widths = self.widths.iter().filter(|_| self.panner.mode != PanMode::Dual);
        self.pans.iter().chain(widths).chain(dual)
    }

    fn pan_matrix(&self, t: f64) -> [f64; 4] {
        let sum = |envs: &[Envelope]| envs.iter().map(|e| e.value_at(t).unwrap_or(0.0)).sum::<f64>();
        let mut p = self.panner;
        p.pan = (p.pan - sum(&self.pans)).clamp(-1.0, 1.0);
        p.width = self.widths.iter().fold(p.width, |w, e| w * e.value_at(t).unwrap_or(1.0));
        for (k, envs) in self.dual.iter().enumerate() {
            p.dual[k] = (p.dual[k] - sum(envs)).clamp(-1.0, 1.0);
        }
        p.matrix()
    }

    pub fn gains_at(&self, frame: i64, block_end: i64, sample_rate: f64) -> Matrix {
        let t = frame as f64 / sample_rate;
        let mut vol = self.volume;
        for e in &self.gains {
            vol *= e.value_at(t).unwrap_or(1.0);
        }
        vol *= self.mute(t);
        let m = if self.pan_like().next().is_some() {
            let block = (frame - frame.rem_euclid(Self::PAN_GRID)) as f64 / sample_rate;
            let mut lo = block;
            let mut hi =
                ((frame - frame.rem_euclid(Self::PAN_GRID) + Self::PAN_GRID).min(block_end)) as f64 / sample_rate;
            for env in self.pan_like() {
                if let Some((v0, v1)) = env.bezier_step(t) {
                    lo = lo.max(v0);
                    hi = hi.min(v1);
                }
                if let Some((e0, e1)) = env.item_edges_around(t) {
                    lo = lo.max(e0);
                    hi = hi.min(e1);
                }
            }
            if hi <= lo {
                self.pan_matrix(t)
            } else {
                let k = ((t - lo) / (hi - lo)).clamp(0.0, 1.0);
                let (a, b) = (self.pan_matrix(lo), self.pan_matrix(hi));
                std::array::from_fn(|i| a[i] + (b[i] - a[i]) * k)
            }
        } else {
            self.panner.matrix()
        };
        scaled(m, vol)
    }

    /// Whether `(t0, t1]` needs per-sample evaluation: an envelope has a point there, where
    /// ramping would smear a step, or is inside a bezier segment, which can be nearly vertical.
    pub fn exact_between(&self, t0: f64, t1: f64) -> bool {
        let near = |e: &Envelope, t0: f64| {
            let i = e.points.partition_point(|p| p.time <= t0);
            let in_bezier = i > 0 && i < e.points.len() && matches!(e.points[i - 1].shape, PointShape::Bezier(_));
            in_bezier || e.points.get(i).is_some_and(|p| p.time <= t1) || e.items_between(t0, t1)
        };
        self.gains.iter().chain(self.pan_like()).any(|e| near(e, t0))
            || self.mutes.iter().any(|e| near(e, t0 - MUTE_FADE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scythe_core::groups::GroupParam;

    #[test]
    fn vca_leads_scale_and_offset_followers() {
        let mut p = Project::default();
        let lead = p.add_track("lead");
        let follower = p.add_track("follower");
        let li = p.track_index(lead).unwrap();
        let fi = p.track_index(follower).unwrap();
        p.tracks[li].groups.set(GroupParam::VcaLead, 0, true);
        p.tracks[fi].groups.set(GroupParam::VcaFollow, 0, true);
        (p.tracks[li].volume, p.tracks[li].pan) = (0.5, 0.3);
        p.tracks[fi].volume = 0.8;
        let f = FaderStage::new(&p, fi);
        assert!((f.volume - 0.4).abs() < 1e-9);
        assert!((f.matrix[0] - 0.4 * 0.7).abs() < 1e-6 && (f.matrix[3] - 0.4).abs() < 1e-6);
        p.tracks[fi].groups.set(GroupParam::VcaFollowPreFx, 0, true);
        p.tracks[fi].phase = true;
        let f = FaderStage::new(&p, fi);
        assert!((f.volume + 0.8).abs() < 1e-9 && f.pre_fx.as_ref().is_some_and(|g| g.gain == 0.5));
    }

    #[test]
    fn vca_pan_glides_in_at_playback_start() {
        // REAPER 7.73, `vp_lm5w`: the lead in stereo pan at 0.3 with width 0.5, the follower
        // centred; a left-only input at 0.5.
        let mut p = Project::default();
        let lead = p.add_track("lead");
        let follower = p.add_track("follower");
        let (li, fi) = (p.track_index(lead).unwrap(), p.track_index(follower).unwrap());
        p.tracks[li].groups.set(GroupParam::VcaLead, 0, true);
        p.tracks[fi].groups.set(GroupParam::VcaFollow, 0, true);
        p.tracks[li].pan_mode = Some(PanMode::Stereo);
        (p.tracks[li].pan, p.tracks[li].width) = (0.3, 0.5);
        let f = FaderStage::new(&p, fi);
        let ramp = f.vca_ramp.expect("a ramp");
        let out = |frames: i64| ramp.matrix_at(frames).map(|g| g * 0.5);
        for (frames, l, r) in
            [(0, 0.5, 0.0), (1, 0.499_463_02, 0.000_244_141), (256, 0.371875, 0.0625), (512, 0.2625, 0.125)]
        {
            let m = out(frames);
            assert!((m[0] - l).abs() < 1e-6 && (m[2] - r).abs() < 1e-6, "{frames}: {m:?}");
        }
        assert_eq!(ramp.matrix_at(512), f.matrix);
        p.tracks[li].pan = 0.0;
        p.tracks[li].width = 1.0;
        assert!(FaderStage::new(&p, fi).vca_ramp.is_none());
    }
}
