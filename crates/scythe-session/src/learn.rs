//! MIDI and OSC learn: controls bound to effect parameters (`PARMLEARN` lines beside each
//! plugin in the project) and to actions (`KEY` lines in `reaper-kb.ini`, as REAPER keeps MIDI
//! shortcuts), and listening for the next message to bind.
//!
//! Measured in REAPER 7.73: a learned controller sets the parameter to value/127 (pitch bend
//! over 14 bits), relative modes step it by 1/762 of its range per count, toggle flips it on
//! any non-zero value, soft takeover waits until the control crosses the parameter's value, and
//! note-offs leave a learned note's parameter alone. A `KEY` line with a MIDI status runs its
//! action on every matching message, controller values of 0 included, but not on note-offs.

use std::path::PathBuf;

use schemars::JsonSchema;
use scythe_core::keymap::{Chord, KeyBinding, Keymap, Line};
use scythe_core::modulation::{self, LEARN_FOCUSED_FX, LEARN_SELECTED_TRACK, LEARN_VISIBLE_FX, ParamLearn};
use scythe_core::rpp::{quote, tokenize};
use scythe_core::{FxSlot, TrackId};
use scythe_engine::modulation::{Control, apply_learned};
use serde::{Deserialize, Serialize};

use crate::Session;

/// Where REAPER keeps shortcuts; Scythe keeps its MIDI and OSC action bindings there too,
/// beside the preferences.
pub const KB_FILE: &str = "reaper-kb.ini";

/// The key REAPER does not use, under which OSC action bindings are kept in [`KB_FILE`].
const OSC_KEY: &str = "SCYTHE_OSC";

/// A message heard while listening.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Heard {
    /// A channel message: its status byte (type and channel) and first data byte.
    Midi { status: u8, data1: u8, device: u8 },
    /// An OSC message from a surface that allows learning.
    Osc { address: String },
}

impl Heard {
    pub fn describe(&self) -> String {
        match self {
            Self::Midi { status, data1, .. } => describe_midi(*status, *data1),
            Self::Osc { address } => format!("OSC {address}"),
        }
    }
}

/// "CC 7, ch 1", "Note 60, ch 10", "Pitch bend, ch 2".
pub fn describe_midi(status: u8, data1: u8) -> String {
    let ch = (status & 0x0f) + 1;
    match status & 0xf0 {
        0x90 | 0x80 => format!("Note {data1}, ch {ch}"),
        0xa0 => format!("Aftertouch {data1}, ch {ch}"),
        0xb0 => format!("CC {data1}, ch {ch}"),
        0xc0 => format!("Program change, ch {ch}"),
        0xd0 => format!("Channel pressure, ch {ch}"),
        0xe0 => format!("Pitch bend, ch {ch}"),
        _ => format!("MIDI {status:02X} {data1:02X}"),
    }
}

/// What runs an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    /// A MIDI message: status byte (type and channel) and first data byte (controller or
    /// note number; 0 for pitch bend and the like).
    Midi { status: u8, data1: u8 },
    /// An OSC address from a surface that allows learning.
    Osc { address: String },
}

impl Trigger {
    pub fn describe(&self) -> String {
        match self {
            Self::Midi { status, data1 } => describe_midi(*status, *data1),
            Self::Osc { address } => format!("OSC {address}"),
        }
    }

    fn matches_midi(&self, msg: [u8; 3]) -> bool {
        let Self::Midi { status, data1 } = *self else { return false };
        let kind = msg[0] & 0xf0;
        if msg[0] != status || kind == 0x80 || kind == 0x90 && msg[2] == 0 {
            return false;
        }
        matches!(kind, 0xc0 | 0xd0 | 0xe0) || msg[1] == data1
    }
}

impl From<&Heard> for Trigger {
    fn from(h: &Heard) -> Self {
        match h {
            Heard::Midi { status, data1, .. } => Self::Midi { status: *status, data1: *data1 },
            Heard::Osc { address } => Self::Osc { address: address.clone() },
        }
    }
}

/// A shortcut from a control to an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ActionBinding {
    pub trigger: Trigger,
    /// A REAPER command id (`40364`) or a named command (`_SCYTHE_SETLIST_NEXT`).
    pub action: String,
}

#[derive(Debug, Default)]
pub struct LearnState {
    /// Waiting for the next message to bind.
    pub listening: bool,
    /// The last message heard while listening.
    pub heard: Option<Heard>,
    pub bindings: Vec<ActionBinding>,
    /// Effects whose window is open, and the one in front, for bindings that only work then:
    /// (track or `None` for the master, chain position).
    pub open_fx: Vec<(Option<TrackId>, usize)>,
    pub focused_fx: Option<(Option<TrackId>, usize)>,
    /// MIDI editor step input: note-ons from the MIDI inputs and the virtual keyboard wait
    /// here for the editor while it is on.
    pub step_input: Option<Vec<[u8; 3]>>,
}

impl LearnState {
    /// Whether a line of the shortcut file is one of these bindings.
    fn is_binding(line: &Line) -> bool {
        match line {
            Line::Key(k) => k.section == 0 && k.chord.is_midi() && k.chord.key < 128,
            Line::Other(l) => l.split_whitespace().next() == Some(OSC_KEY),
            _ => false,
        }
    }

    /// Takes the `KEY` lines with a MIDI status (section 0) and Scythe's OSC lines out of
    /// `keymap`.
    pub fn take_from(&mut self, keymap: &mut Keymap) {
        let text: String = keymap.lines.iter().filter(|l| Self::is_binding(l)).map(|l| l.to_text() + "\n").collect();
        keymap.lines.retain(|l| !Self::is_binding(l));
        self.parse(&text);
    }

    /// Reads `KEY` lines with a MIDI status (section 0) and Scythe's OSC lines.
    pub fn parse(&mut self, text: &str) {
        self.bindings.clear();
        for line in text.lines() {
            let t = tokenize(line.split('#').next().unwrap_or(""));
            let binding = match t.first().map(String::as_str) {
                Some("KEY") if t.len() >= 5 && t[4] == "0" => match (t[1].parse::<u32>(), t[2].parse::<u32>()) {
                    (Ok(status), Ok(data1)) if (0x80..0xf0).contains(&status) && data1 < 128 => Some(ActionBinding {
                        trigger: Trigger::Midi { status: status as u8, data1: data1 as u8 },
                        action: t[3].clone(),
                    }),
                    _ => None,
                },
                Some(OSC_KEY) if t.len() >= 3 => {
                    Some(ActionBinding { trigger: Trigger::Osc { address: t[1].clone() }, action: t[2].clone() })
                }
                _ => None,
            };
            if let Some(b) = binding {
                self.bindings.push(b);
            }
        }
    }

    /// Puts the bindings into `keymap` where REAPER keeps them, `name` giving each action's
    /// name for the comment REAPER writes.
    pub fn put_into(&self, keymap: &mut Keymap, name: impl Fn(&str) -> Option<String>) {
        for b in &self.bindings {
            match &b.trigger {
                Trigger::Midi { status, data1 } => {
                    let chord = Chord::new(u32::from(*status), u32::from(*data1));
                    let comment = format!("Main : {} : {}", chord.describe(), name(&b.action).unwrap_or_default());
                    keymap.insert_key(KeyBinding {
                        chord,
                        command: b.action.clone(),
                        section: 0,
                        comment: Some(comment),
                    });
                }
                Trigger::Osc { address } => {
                    keymap.lines.push(Line::Other(format!("{OSC_KEY} {} {}", quote(address), quote(&b.action))));
                }
            }
        }
    }
}

/// Every effect slot of the project with where it sits: (track or `None` for the master,
/// chain position, slot).
fn slots(project: &scythe_core::Project) -> impl Iterator<Item = (Option<TrackId>, usize, &FxSlot)> {
    std::iter::once((None, &project.master.fx))
        .chain(project.tracks.iter().map(|t| (Some(t.id), &t.fx)))
        .flat_map(|(track, fx)| fx.iter().enumerate().map(move |(i, s)| (track, i, s)))
}

impl Session {
    pub(crate) fn kb_path(&self) -> Option<PathBuf> {
        Some(self.prefs_file.as_ref()?.with_file_name(KB_FILE))
    }

    /// Reads the shortcut file beside the preferences: the MIDI and OSC bindings into
    /// `learn`, the rest into `keymap`.
    pub(crate) fn load_action_bindings(&mut self) {
        let Some(path) = self.kb_path() else { return };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut keymap = Keymap::parse(&text);
        self.learn.take_from(&mut keymap);
        self.keymap = keymap;
    }

    /// The whole shortcut file as it is saved.
    pub fn keymap_text(&self) -> String {
        let mut keymap = self.keymap.clone();
        self.learn.put_into(&mut keymap, |a| self.action_name(a));
        keymap.write()
    }

    pub(crate) fn save_action_bindings(&mut self) {
        let Some(path) = self.kb_path() else { return };
        if let Err(e) = std::fs::write(&path, self.keymap_text()) {
            self.notify(format!("Cannot save {}: {e}", path.display()));
        }
    }

    /// Binds `trigger` to `action`, replacing what the trigger ran before.
    pub fn bind_action(&mut self, trigger: Trigger, action: String) {
        self.learn.bindings.retain(|b| b.trigger != trigger);
        self.learn.bindings.push(ActionBinding { trigger, action });
        self.save_action_bindings();
        self.sync();
        self.changed();
    }

    /// Removes the bindings `keep` refuses; returns how many went.
    pub fn unbind_actions(&mut self, keep: impl Fn(&ActionBinding) -> bool) -> usize {
        let before = self.learn.bindings.len();
        self.learn.bindings.retain(|b| keep(b));
        let removed = before - self.learn.bindings.len();
        if removed > 0 {
            self.save_action_bindings();
            self.sync();
            self.changed();
        }
        removed
    }

    /// Waits for the next MIDI or OSC message, to bind it.
    pub fn listen_for_control(&mut self, on: bool) {
        self.learn.listening = on;
        if on {
            self.learn.heard = None;
        }
        self.sync();
    }

    /// Whether the MIDI inputs enabled for control need to be open.
    pub(crate) fn control_inputs_wanted(&self) -> bool {
        self.learn.listening
            || self.learn.step_input.is_some()
            || self.learn.bindings.iter().any(|b| matches!(b.trigger, Trigger::Midi { .. }))
            || slots(&self.project).any(|(_, _, s)| modulation::learns(s).iter().any(|l| l.status() >= 0x80))
    }

    /// Handles the messages from the MIDI inputs: listening, learned parameters, then action
    /// shortcuts. Call it every frame.
    pub fn poll_control(&mut self) {
        let messages = self.engine.take_control_midi();
        for m in messages {
            if !self.prefs.midi.control_enabled(m.device) || m.len == 0 || m.bytes[0] >= 0xf0 {
                continue;
            }
            let msg = m.bytes;
            if let Some(steps) = &mut self.learn.step_input
                && msg[0] & 0xf0 == 0x90
                && msg[2] > 0
            {
                steps.push(msg);
            }
            if self.learn.listening {
                let kind = msg[0] & 0xf0;
                if kind == 0x80 || kind == 0x90 && msg[2] == 0 {
                    continue;
                }
                let data1 = if matches!(kind, 0xc0 | 0xd0 | 0xe0) { 0 } else { msg[1] };
                self.learn.heard = Some(Heard::Midi { status: msg[0], data1, device: m.device });
                self.learn.listening = false;
                self.sync();
                self.changed();
                continue;
            }
            self.learned_controls(|l| l.matches(msg), Control::Midi(msg));
            let actions: Vec<String> =
                self.learn.bindings.iter().filter(|b| b.trigger.matches_midi(msg)).map(|b| b.action.clone()).collect();
            for a in actions {
                if let Err(e) = crate::actions::run(self, &a) {
                    self.notify(format!("MIDI shortcut: {e}"));
                }
            }
        }
    }

    /// An OSC message from a surface that allows learning: heard while listening (then it
    /// does nothing else), else applied to learned parameters and run as a bound action.
    /// Returns whether listening took it.
    pub fn control_osc(&mut self, address: &str, value: Option<f64>) -> bool {
        if self.learn.listening {
            self.learn.heard = Some(Heard::Osc { address: address.to_owned() });
            self.learn.listening = false;
            self.sync();
            self.changed();
            return true;
        }
        if let Some(v) = value {
            self.learned_controls(|l| !l.osc.is_empty() && l.osc == address, Control::Value(v));
        }
        let actions: Vec<String> = self
            .learn
            .bindings
            .iter()
            .filter(|b| matches!(&b.trigger, Trigger::Osc { address: a } if a == address))
            .map(|b| b.action.clone())
            .collect();
        // A release (0) of a button bound to an action does nothing, as with OSC actions.
        if value.is_none_or(|v| v > 0.0) {
            for a in actions {
                if let Err(e) = crate::actions::run(self, &a) {
                    self.notify(format!("OSC shortcut: {e}"));
                }
            }
        }
        false
    }

    /// Applies `control` to every learned parameter `matches` accepts, honouring the
    /// selected-track and open-window conditions.
    fn learned_controls(&mut self, matches: impl Fn(&ParamLearn) -> bool, control: Control) {
        let t = self.position();
        let now = (t, self.project.tempo.timeline().qn_at(t));
        let mut hits = Vec::new();
        for (track, i, slot) in slots(&self.project) {
            for l in modulation::learns(slot).into_iter().filter(|l| matches(l)) {
                if l.flags & LEARN_SELECTED_TRACK != 0 && (track.is_none() || track != self.selected_track) {
                    continue;
                }
                if l.flags & LEARN_FOCUSED_FX != 0 && self.learn.focused_fx != Some((track, i)) {
                    continue;
                }
                if l.flags & LEARN_VISIBLE_FX != 0 && !self.learn.open_fx.contains(&(track, i)) {
                    continue;
                }
                let m = l.index().and_then(|p| modulation::modulation(slot, p)).filter(|m| m.is_active());
                hits.push((track, i, slot.clone(), l, m));
            }
        }
        let mut changed = false;
        for (track, i, slot, l, m) in hits {
            let Some(inst) = self.engine.fx().instance(track, &slot, i) else { continue };
            changed |= apply_learned(&inst, &l, control, m.as_ref(), now).is_some();
        }
        if changed {
            self.engine.wake_effects();
            self.dirty = true;
            self.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_file_round_trip() {
        let text = "SCR 4 0 RS1234 \"Custom: x\" foo.lua\nKEY 176 1 40364 0\nKEY 1 65 40044 0\nKEY 144 60 _SCYTHE_SETLIST_NEXT 0\nSCYTHE_OSC /foot/1 40364\n";
        let mut s = LearnState::default();
        s.parse(text);
        assert_eq!(s.bindings.len(), 3);
        assert_eq!(s.bindings[0].trigger, Trigger::Midi { status: 176, data1: 1 });
        assert_eq!(s.bindings[1].action, "_SCYTHE_SETLIST_NEXT");
        assert_eq!(s.bindings[2].trigger, Trigger::Osc { address: "/foot/1".into() });
        let mut keymap = Keymap::parse(text);
        let mut again = LearnState::default();
        again.take_from(&mut keymap);
        assert_eq!(again.bindings, s.bindings);
        assert_eq!(keymap.write(), "SCR 4 0 RS1234 \"Custom: x\" foo.lua\nKEY 1 65 40044 0\n");
        again.put_into(&mut keymap, |a| (a == "40364").then(|| "Options: Toggle metronome".to_owned()));
        assert_eq!(
            keymap.write(),
            "SCR 4 0 RS1234 \"Custom: x\" foo.lua\nKEY 1 65 40044 0\nKEY 176 1 40364 0\t\t # Main : MIDI Chan 1 CC 1 : Options: Toggle metronome\nKEY 144 60 _SCYTHE_SETLIST_NEXT 0\t\t # Main : MIDI Chan 1 Note 60 : \nSCYTHE_OSC /foot/1 40364\n"
        );
    }

    #[test]
    fn midi_shortcuts_match_as_reaper_runs_them() {
        let cc = Trigger::Midi { status: 0xb0, data1: 64 };
        assert!(cc.matches_midi([0xb0, 64, 127]) && cc.matches_midi([0xb0, 64, 0]));
        assert!(!cc.matches_midi([0xb1, 64, 127]) && !cc.matches_midi([0xb0, 65, 127]));
        let note = Trigger::Midi { status: 0x90, data1: 60 };
        assert!(note.matches_midi([0x90, 60, 100]));
        assert!(!note.matches_midi([0x80, 60, 0]) && !note.matches_midi([0x90, 60, 0]));
    }
}
