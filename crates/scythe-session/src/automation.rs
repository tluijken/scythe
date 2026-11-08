use std::collections::HashMap;

use scythe_core::{AutoMode, EnvelopeKind, TrackId};

use crate::Session;

/// A control that automation writing follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoTarget {
    Volume,
    Pan,
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

impl Session {
    /// Routes a control move on `track` into its envelope when the track writes automation and
    /// the transport runs. Returns false when the move should change the control itself.
    pub fn automate(&mut self, track: TrackId, target: AutoTarget, value: f64) -> bool {
        let writes = self.project.track(track).is_some_and(|t| t.auto_mode.writes());
        if !writes || !self.is_playing() {
            return false;
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

    /// Writes the controls of this pass at the play position: touched ones in touch mode,
    /// everything moved since the start in latch mode, and every control in write mode. The
    /// pass becomes one undo step when playback stops.
    pub fn tick_automation(&mut self) {
        if !self.is_playing() {
            if !self.auto_writes.is_empty() {
                self.auto_writes.clear();
                self.end_gesture();
            }
            return;
        }
        let now = self.engine.transport().position_seconds();
        // Write mode records both controls even when nobody touches them.
        for t in self.project.tracks.iter().filter(|t| t.auto_mode == AutoMode::Write) {
            for target in [AutoTarget::Volume, AutoTarget::Pan] {
                if !self.auto_writes.iter().any(|w| w.track == t.id && w.target == target) {
                    let value = match target {
                        AutoTarget::Volume => {
                            t.envelope(EnvelopeKind::Volume).and_then(|e| e.value_at(now)).unwrap_or(1.0)
                        }
                        AutoTarget::Pan => {
                            t.pan - t.envelope(EnvelopeKind::Pan).and_then(|e| e.value_at(now)).unwrap_or(0.0)
                        }
                    };
                    self.auto_writes.push(AutoWrite { track: t.id, target, value, touching: false, last: None });
                }
            }
        }
        let mut writes = std::mem::take(&mut self.auto_writes);
        let modes: HashMap<TrackId, AutoMode> = self.project.tracks.iter().map(|t| (t.id, t.auto_mode)).collect();
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
                    let Some(t) = p.track_mut(w.track) else { continue };
                    let (kind, value) = match w.target {
                        AutoTarget::Volume => (EnvelopeKind::Volume, w.value),
                        // The pan envelope is subtracted from the track pan.
                        AutoTarget::Pan => (EnvelopeKind::Pan, t.pan - w.value),
                    };
                    let env = t.envelope_mut(kind);
                    let from = w.last.unwrap_or(now - 1e-6);
                    env.points.retain(|pt| pt.time <= from || pt.time > now);
                    env.points.push(scythe_core::EnvPoint::new(now, value));
                    env.sort();
                }
            });
            for &i in &active {
                writes[i].last = Some(now);
            }
        }
        self.auto_writes = writes;
    }
}
