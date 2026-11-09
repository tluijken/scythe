use std::collections::HashMap;

use scythe_core::fx_chain::ParamEnvelope;
use scythe_core::{AutoMode, EnvPoint, Envelope, EnvelopeKind, PointShape, Project, TrackId};

use crate::Session;

/// A control that automation writing follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoTarget {
    Volume,
    Pan,
    Width,
    /// 1 plays, 0 mutes.
    Mute,
    /// A send's volume (gain), by its index on the track.
    SendVolume(usize),
    SendPan(usize),
    SendMute(usize),
    /// An effect's parameter in its own units.
    Param {
        fx: usize,
        param: usize,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct AutoWrite {
    track: TrackId,
    target: AutoTarget,
    /// The control's value (volume as gain, pan -1..=1).
    value: f64,
    touching: bool,
    /// Time of the last written point; points after it are replaced as the pass moves on.
    last: Option<f64>,
}

/// A value a latch preview pass remembered, for `apply_latch_preview` to write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Remembered {
    pub track: TrackId,
    pub target: AutoTarget,
    pub value: f64,
}

/// Where `apply_latch_preview` writes the remembered values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSpan {
    TimeSelection,
    CursorToEnd,
    StartToCursor,
}

fn kind_of(target: AutoTarget) -> Option<EnvelopeKind> {
    match target {
        AutoTarget::Volume | AutoTarget::SendVolume(_) => Some(EnvelopeKind::Volume),
        AutoTarget::Pan | AutoTarget::SendPan(_) => Some(EnvelopeKind::Pan),
        AutoTarget::Width => Some(EnvelopeKind::Width),
        AutoTarget::Mute | AutoTarget::SendMute(_) => Some(EnvelopeKind::Mute),
        AutoTarget::Param { .. } => None,
    }
}

/// Runs `f` on the envelope a target writes into, or not at all when that envelope exists
/// but is not armed (REAPER writes armed envelopes only). Missing envelopes are created,
/// armed.
fn with_envelope<R>(
    project: &mut Project,
    track: TrackId,
    target: AutoTarget,
    f: impl FnOnce(&mut Envelope) -> R,
) -> Option<R> {
    let t = project.track_mut(track)?;
    match target {
        AutoTarget::SendVolume(s) | AutoTarget::SendPan(s) | AutoTarget::SendMute(s) => {
            let kind = kind_of(target)?;
            let send = t.sends.get_mut(s)?;
            let env = match send.envelopes.iter().position(|e| e.kind == kind) {
                Some(i) => &mut send.envelopes[i],
                None => {
                    send.envelopes.push(Envelope { fader_scaled: kind.is_volume(), ..Envelope::new(kind) });
                    send.envelopes.last_mut()?
                }
            };
            env.armed.then(|| f(env))
        }
        AutoTarget::Param { fx, param } => {
            let slot = t.fx.get_mut(fx)?;
            let mut pe = slot
                .parameter_envelope(&param.to_string())
                .unwrap_or_else(|| ParamEnvelope::new(param, 0.0, 1.0, 0.0, ""));
            if !pe.envelope.armed {
                return None;
            }
            let out = f(&mut pe.envelope);
            slot.set_parameter_envelope(&pe);
            Some(out)
        }
        _ => {
            let kind = kind_of(target)?;
            let exists = t.envelope(kind).is_some();
            let env = t.envelope_mut(kind);
            (env.armed || !exists).then(|| f(env))
        }
    }
}

/// What the envelope stores for a control at `value`: pan is subtracted from the track's,
/// width multiplies it.
fn stored(project: &Project, track: TrackId, target: AutoTarget, value: f64) -> f64 {
    let Some(t) = project.track(track) else { return value };
    match target {
        AutoTarget::Pan => t.pan - value,
        AutoTarget::Width if t.width.abs() > 1e-9 => value / t.width,
        AutoTarget::SendPan(s) => t.sends.get(s).map_or(0.0, |send| send.pan) - value,
        _ => value,
    }
}

/// The control's value at `now` as the envelopes play it, in the control's units.
fn current(project: &Project, track: TrackId, target: AutoTarget, now: f64) -> f64 {
    let Some(t) = project.track(track) else { return 0.0 };
    let env = |k: EnvelopeKind| t.envelope(k).and_then(|e| e.value_at(now));
    match target {
        AutoTarget::Volume => env(EnvelopeKind::Volume).unwrap_or(1.0),
        AutoTarget::Pan => t.pan - env(EnvelopeKind::Pan).unwrap_or(0.0),
        AutoTarget::Width => t.width * env(EnvelopeKind::Width).unwrap_or(1.0),
        AutoTarget::Mute => env(EnvelopeKind::Mute).unwrap_or(if t.muted { 0.0 } else { 1.0 }),
        AutoTarget::SendVolume(s) | AutoTarget::SendPan(s) | AutoTarget::SendMute(s) => {
            let Some(send) = t.sends.get(s) else { return 0.0 };
            let kind = kind_of(target).unwrap_or(EnvelopeKind::Volume);
            let v = send.envelopes.iter().find(|e| e.kind == kind).and_then(|e| e.value_at(now));
            match target {
                AutoTarget::SendPan(_) => send.pan - v.unwrap_or(0.0),
                AutoTarget::SendMute(_) => v.unwrap_or(if send.muted { 0.0 } else { 1.0 }),
                _ => v.unwrap_or(1.0),
            }
        }
        AutoTarget::Param { fx, param } => {
            t.fx.get(fx)
                .and_then(|s| s.parameter_envelope(&param.to_string()))
                .and_then(|p| p.envelope.value_at(now))
                .unwrap_or(0.0)
        }
    }
}

impl Session {
    /// Routes a control move on `track` into its envelope when the track writes automation (or
    /// remembers it, in latch preview) and the transport runs. Returns false when the move
    /// should change the control itself.
    pub fn automate(&mut self, track: TrackId, target: AutoTarget, value: f64) -> bool {
        let Some(mode) = self.project.track(track).map(|t| self.project.auto_mode_of(t)) else { return false };
        if !(mode.writes() || mode == AutoMode::LatchPreview) || !self.is_playing() {
            return false;
        }
        if let AutoTarget::Param { fx, param } = target {
            self.ensure_parameter_envelope(track, fx, param);
        }
        match self.auto_writes.iter_mut().find(|w| w.track == track && w.target == target) {
            Some(w) => {
                w.value = value;
                w.touching = true;
            }
            None => self.auto_writes.push(AutoWrite { track, target, value, touching: true, last: None }),
        }
        true
    }

    /// Creates an FX parameter envelope over the parameter's own range, as part of the pass.
    fn ensure_parameter_envelope(&mut self, track: TrackId, fx: usize, param: usize) {
        let spec = param.to_string();
        let exists = |p: &Project| {
            p.track(track).and_then(|t| t.fx.get(fx)).is_none_or(|s| s.parameter_envelope(&spec).is_some())
        };
        if exists(&self.project) {
            return;
        }
        let target = crate::api::Target::Track(track);
        let Ok((label, lo, hi, v)) = crate::api::envelope::param_info(self, target, fx, param) else { return };
        self.gesture("Write automation", |p| {
            if let Some(slot) = p.track_mut(track).and_then(|t| t.fx.get_mut(fx)) {
                slot.set_parameter_envelope(&ParamEnvelope::new(param, lo, hi, v, &label));
            }
        });
    }

    /// The control was let go: touch mode stops writing it.
    pub fn release_automation(&mut self, track: TrackId, target: AutoTarget) {
        if let Some(w) = self.auto_writes.iter_mut().find(|w| w.track == track && w.target == target) {
            w.touching = false;
        }
    }

    /// The value being written for a control, for the mixer to show instead of the stored one.
    pub fn automation_value(&self, track: TrackId, target: AutoTarget) -> Option<f64> {
        self.auto_writes.iter().find(|w| w.track == track && w.target == target).map(|w| w.value)
    }

    /// The values the last latch preview pass remembered.
    pub fn latch_preview(&self) -> &[Remembered] {
        &self.latch_preview
    }

    /// Writes the values a latch preview pass remembered over `span` as one undo step and
    /// forgets them. Returns how many envelopes were written.
    pub fn apply_latch_preview(&mut self, span: PreviewSpan) -> usize {
        let cursor = self.project.cursor;
        let end = self.project.tracks.iter().flat_map(|t| &t.items).map(|i| i.end()).fold(cursor, f64::max);
        let (from, to) = match span {
            PreviewSpan::TimeSelection => match self.project.time_selection {
                Some(sel) => (sel.start, sel.end),
                None => return 0,
            },
            PreviewSpan::CursorToEnd => (cursor, end.max(cursor + 1.0)),
            PreviewSpan::StartToCursor => (0.0, cursor),
        };
        if to <= from {
            return 0;
        }
        let values = std::mem::take(&mut self.latch_preview);
        let mut written = 0;
        self.edit("Write latch preview values", |p| {
            for r in &values {
                let v = stored(p, r.track, r.target, r.value);
                let done = with_envelope(p, r.track, r.target, |env| {
                    let (before, after) = (env.value_at(from), env.value_at(to));
                    env.points.retain(|pt| pt.time < from || pt.time > to);
                    env.points.extend(before.map(|b| EnvPoint::new(from, b)));
                    env.points.push(EnvPoint::new(from, v));
                    env.points.push(EnvPoint::new(to, v));
                    env.points.extend(after.map(|a| EnvPoint::new(to, a)));
                    env.sort();
                });
                written += done.is_some() as usize;
            }
        });
        written
    }

    /// Writes the controls of this pass at the play position: touched ones in touch mode,
    /// everything moved since the start in latch mode, and every armed control in write mode.
    /// The pass becomes one undo step when playback stops. Latch preview only remembers.
    pub fn tick_automation(&mut self) {
        let modes: HashMap<TrackId, AutoMode> =
            self.project.tracks.iter().map(|t| (t.id, self.project.auto_mode_of(t))).collect();
        if !self.is_playing() {
            if !self.auto_writes.is_empty() {
                for w in &self.auto_writes {
                    if modes.get(&w.track) == Some(&AutoMode::LatchPreview) {
                        self.latch_preview.retain(|r| (r.track, r.target) != (w.track, w.target));
                        self.latch_preview.push(Remembered { track: w.track, target: w.target, value: w.value });
                    }
                }
                self.auto_writes.clear();
                self.end_gesture();
            }
            return;
        }
        let now = self.engine.transport().position_seconds();
        // Write mode records volume and pan, and every other armed envelope, even when nobody
        // touches them.
        let mut fresh = Vec::new();
        for t in self.project.tracks.iter().filter(|t| modes.get(&t.id) == Some(&AutoMode::Write)) {
            let armed = |k: EnvelopeKind| t.envelope(k).is_some_and(|e| e.armed);
            let mut targets = vec![AutoTarget::Volume, AutoTarget::Pan];
            targets.extend(armed(EnvelopeKind::Width).then_some(AutoTarget::Width));
            targets.extend(armed(EnvelopeKind::Mute).then_some(AutoTarget::Mute));
            for (s, send) in t.sends.iter().enumerate() {
                targets.extend(send.envelopes.iter().filter(|e| e.armed).filter_map(|e| match e.kind {
                    EnvelopeKind::Volume => Some(AutoTarget::SendVolume(s)),
                    EnvelopeKind::Pan => Some(AutoTarget::SendPan(s)),
                    EnvelopeKind::Mute => Some(AutoTarget::SendMute(s)),
                    _ => None,
                }));
            }
            for (fx, slot) in t.fx.iter().enumerate() {
                let params = slot.parameter_envelopes();
                let armed = params.into_iter().filter(|p| p.envelope.armed).filter_map(|p| p.param());
                targets.extend(armed.map(|param| AutoTarget::Param { fx, param }));
            }
            for target in targets {
                if !self.auto_writes.iter().any(|w| w.track == t.id && w.target == target) {
                    let value = current(&self.project, t.id, target, now);
                    fresh.push(AutoWrite { track: t.id, target, value, touching: false, last: None });
                }
            }
        }
        self.auto_writes.extend(fresh);
        let mut writes = std::mem::take(&mut self.auto_writes);
        let active: Vec<usize> = writes
            .iter()
            .enumerate()
            .filter(|(_, w)| match modes.get(&w.track) {
                Some(AutoMode::Touch) => w.touching,
                Some(AutoMode::Latch | AutoMode::Write) => true,
                _ => false,
            })
            .map(|(i, _)| i)
            .collect();
        if !active.is_empty() {
            self.gesture("Write automation", |p| {
                for &i in &active {
                    let w = &writes[i];
                    let value = stored(p, w.track, w.target, w.value);
                    let square = matches!(w.target, AutoTarget::Mute | AutoTarget::SendMute(_));
                    with_envelope(p, w.track, w.target, |env| {
                        let from = w.last.unwrap_or(now - 1e-6);
                        env.points.retain(|pt| pt.time <= from || pt.time > now);
                        let shape = if square { PointShape::Square } else { PointShape::Linear };
                        env.points.push(EnvPoint { shape, ..EnvPoint::new(now, value) });
                        env.sort();
                    });
                }
            });
            for &i in &active {
                writes[i].last = Some(now);
            }
        }
        self.auto_writes = writes;
    }
}
