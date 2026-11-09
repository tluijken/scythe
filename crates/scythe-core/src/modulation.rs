//! Parameter modulation and learn as REAPER keeps them beside a plugin in its FX chain:
//! `<PROGRAMENV` chunks (baseline, LFO, audio control signal, parameter link) and
//! `PARMLEARN`/`LFOLEARN` lines (MIDI or OSC bindings). Lines Scythe does not model survive a
//! rewrite.

use crate::FxSlot;
use crate::rpp::{Chunk, Node, line_key, quote_always, tokenize};

/// LFO waveforms in REAPER's order (`LFOSHAPE`).
pub const LFO_SHAPES: [&str; 6] = ["Sine", "Square", "Saw down", "Saw up", "Triangle", "Random"];

/// Slowest and fastest free-running LFO rates: `LFOSPEED` spreads 0..1 linearly over them.
pub const LFO_MIN_HZ: f64 = 1.0 / 256.0;
pub const LFO_MAX_HZ: f64 = 8.0;

/// A parameter's LFO. The fields hold their values while it is off, but REAPER only stores
/// them while it is on.
#[derive(Debug, Clone, PartialEq)]
pub struct Lfo {
    pub active: bool,
    /// Share of the parameter's range it sweeps, 0..1.
    pub strength: f64,
    /// 1 sweeps up from the baseline, -1 down, 0 around it.
    pub direction: i32,
    /// Index into [`LFO_SHAPES`].
    pub shape: u8,
    pub tempo_sync: bool,
    /// The middle `LFOSYNC` field, kept as read.
    pub sync_flag: i32,
    /// Phase runs from the start of playback instead of the project position.
    pub free: bool,
    /// The stored rate: see [`Lfo::hz`] and [`Lfo::period_qn`].
    pub speed: f64,
    /// Start phase, 0..1 of a cycle.
    pub phase: f64,
}

impl Default for Lfo {
    fn default() -> Self {
        Self {
            active: false,
            strength: 1.0,
            direction: 1,
            shape: 0,
            tempo_sync: false,
            sync_flag: 0,
            free: false,
            speed: Self::speed_for_hz(1.0),
            phase: 0.0,
        }
    }
}

impl Lfo {
    /// Cycles per second when not tempo synced.
    pub fn hz(&self) -> f64 {
        LFO_MIN_HZ + self.speed * (LFO_MAX_HZ - LFO_MIN_HZ)
    }

    pub fn speed_for_hz(hz: f64) -> f64 {
        ((hz - LFO_MIN_HZ) / (LFO_MAX_HZ - LFO_MIN_HZ)).max(0.0)
    }

    /// Quarter notes per cycle when tempo synced: 8 down to 4 over the lower half of the
    /// stored range, 4 down to a quarter of one over the upper half.
    pub fn period_qn(&self) -> f64 {
        if self.speed <= 0.5 { 8.0 - 8.0 * self.speed } else { 4.0 - 7.5 * (self.speed - 0.5) }
    }

    pub fn speed_for_period_qn(qn: f64) -> f64 {
        if qn >= 4.0 { (8.0 - qn) / 8.0 } else { 0.5 + (4.0 - qn) / 7.5 }
    }

    /// The waveform at `phase` (cycles), from 0 to 1. `random` gives the random shape's value
    /// for a cycle number.
    pub fn wave(&self, phase: f64, random: impl Fn(i64) -> f64) -> f64 {
        let p = phase.rem_euclid(1.0);
        match self.shape {
            1 => {
                if p < 0.5 {
                    0.0
                } else {
                    1.0
                }
            }
            2 => 1.0 - p,
            3 => p,
            4 => 1.0 - (2.0 * p - 1.0).abs(),
            5 => {
                // Glides from one random level to the next over each cycle.
                let n = phase.floor() as i64;
                let (a, b) = (random(n), random(n + 1));
                a + (b - a) * p
            }
            _ => (1.0 - (std::f64::consts::TAU * p).cos()) / 2.0,
        }
    }
}

/// Following the level of the effect's input (`AUDIOCTL`).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioControl {
    pub active: bool,
    pub strength: f64,
    pub direction: i32,
    /// First input channel (0-based); -1 listens to nothing.
    pub channel: i32,
    /// Listens to `channel` and the next one, mixed.
    pub stereo: bool,
    pub attack_ms: f64,
    pub release_ms: f64,
    /// Level range mapped onto 0..1.
    pub db_low: f64,
    pub db_high: f64,
    /// Control point of the curve from level to value (0.5, 0.5 is straight).
    pub x2: f64,
    pub y2: f64,
}

impl Default for AudioControl {
    fn default() -> Self {
        Self {
            active: false,
            strength: 1.0,
            direction: 1,
            channel: 0,
            stereo: true,
            attack_ms: 300.0,
            release_ms: 300.0,
            db_low: -24.0,
            db_high: 0.0,
            x2: 0.5,
            y2: 0.5,
        }
    }
}

impl AudioControl {
    /// The 0..1 control value for a follower level (linear amplitude).
    pub fn shape(&self, level: f64) -> f64 {
        let span = self.db_high - self.db_low;
        if level <= 0.0 || span.abs() < 1e-12 {
            return if level > 0.0 && 20.0 * level.log10() >= self.db_high { 1.0 } else { 0.0 };
        }
        let x = ((20.0 * level.log10() - self.db_low) / span).clamp(0.0, 1.0);
        bezier(x, self.x2, self.y2)
    }
}

/// The quadratic Bezier from (0, 0) to (1, 1) through control point (`cx`, `cy`), at `x`.
fn bezier(x: f64, cx: f64, cy: f64) -> f64 {
    // x(t) = 2t(1-t)cx + t^2 solved for t.
    let a = 1.0 - 2.0 * cx;
    let t = if a.abs() < 1e-9 {
        if cx.abs() < 1e-12 { x.sqrt() } else { x / (2.0 * cx) }
    } else {
        (-cx + (cx * cx + a * x).max(0.0).sqrt()) / a
    };
    let t = t.clamp(0.0, 1.0);
    2.0 * t * (1.0 - t) * cy + t * t
}

/// Where a linked parameter takes its value from.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkSource {
    /// A parameter of an effect in the same chain: `fx` as REAPER stores it (`2` or `2:1`,
    /// the second number relative to the linking effect) and the parameter index.
    Fx { fx: String, param: i64 },
    /// A MIDI message on the effect's input (`MIDIPLINK bus channel status data1`); channel 0
    /// is any.
    Midi { bus: i32, channel: i32, status: i32, data1: i32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamLink {
    pub scale: f64,
    pub offset: f64,
    pub source: LinkSource,
    /// The last `PLINK` field, kept as read.
    pub tail: String,
}

impl ParamLink {
    /// The chain position of the source effect for a link on effect `own`.
    pub fn source_fx(&self, own: usize) -> Option<usize> {
        let LinkSource::Fx { fx, .. } = &self.source else { return None };
        let (abs, rel) = match fx.split_once(':') {
            Some((a, r)) => (a.parse::<i64>().ok(), r.parse::<i64>().ok()),
            None => (fx.parse::<i64>().ok(), None),
        };
        let index = match rel {
            Some(r) => own as i64 + r,
            None => abs?,
        };
        usize::try_from(index).ok()
    }

    /// REAPER's spelling of a link to effect `target` from effect `own`.
    pub fn fx_token(target: usize, own: usize) -> String {
        format!("{target}:{}", target as i64 - own as i64)
    }
}

/// A `<PROGRAMENV` chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamModulation {
    /// The parameter as REAPER names it: `3` or `3:_Freq` (index and name).
    pub param: String,
    /// Modulation switched off but kept.
    pub disabled: bool,
    pub label: String,
    /// Value without modulation, in the parameter's own units.
    pub base: f64,
    pub lfo: Lfo,
    pub acs: AudioControl,
    pub link: Option<ParamLink>,
    /// Link switched off but kept (REAPER drops it; kept so a toggle does not lose it).
    pub link_active: bool,
    /// Lines Scythe does not model.
    pub extra: Vec<Node>,
}

impl ParamModulation {
    pub fn new(param: impl Into<String>, label: impl Into<String>, base: f64) -> Self {
        Self {
            param: param.into(),
            disabled: false,
            label: label.into(),
            base,
            lfo: Lfo::default(),
            acs: AudioControl::default(),
            link: None,
            link_active: false,
            extra: Vec::new(),
        }
    }

    pub fn index(&self) -> Option<usize> {
        param_index(&self.param)
    }

    /// Whether any source moves the parameter.
    pub fn is_active(&self) -> bool {
        !self.disabled && (self.lfo.active || self.acs.active || self.link_active && self.link.is_some())
    }

    pub fn from_chunk(c: &Chunk) -> Option<Self> {
        if c.name() != "PROGRAMENV" {
            return None;
        }
        let args = c.args();
        let mut m = Self::new(args.first()?.clone(), args.get(2).cloned().unwrap_or_default(), 0.0);
        m.disabled = args.get(1).is_some_and(|v| v != "0");
        let mut link = None;
        let mut midi = None;
        for node in &c.children {
            let Node::Line(l) = node else {
                m.extra.push(node.clone());
                continue;
            };
            let t = tokenize(l);
            let f = |i: usize, d: f64| t.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
            let i = |k: usize, d: i32| t.get(k).and_then(|v| v.parse::<f64>().ok()).map_or(d, |v| v as i32);
            match t.first().map(String::as_str).unwrap_or("") {
                "PARAMBASE" => m.base = f(1, 0.0),
                "LFO" => m.lfo.active = f(1, 0.0) != 0.0,
                "LFOWT" => (m.lfo.strength, m.lfo.direction) = (f(1, 1.0), i(2, 1)),
                "AUDIOCTL" => m.acs.active = f(1, 0.0) != 0.0,
                "AUDIOCTLWT" => (m.acs.strength, m.acs.direction) = (f(1, 1.0), i(2, 1)),
                "LFOSHAPE" => m.lfo.shape = i(1, 0).clamp(0, 5) as u8,
                "LFOSYNC" => {
                    m.lfo.tempo_sync = i(1, 0) != 0;
                    m.lfo.sync_flag = i(2, 0);
                    m.lfo.free = i(3, 0) != 0;
                }
                "LFOSPEED" => (m.lfo.speed, m.lfo.phase) = (f(1, 0.124573), f(2, 0.0)),
                "CHAN" => m.acs.channel = i(1, -1),
                "STEREO" => m.acs.stereo = i(1, 0) != 0,
                "RMS" => (m.acs.attack_ms, m.acs.release_ms) = (f(1, 300.0), f(2, 300.0)),
                "DBLO" => m.acs.db_low = f(1, -24.0),
                "DBHI" => m.acs.db_high = f(1, 0.0),
                "X2" => m.acs.x2 = f(1, 0.5),
                "Y2" => m.acs.y2 = f(1, 0.5),
                "PLINK" => {
                    link = Some((
                        f(1, 1.0),
                        t.get(2).cloned().unwrap_or_else(|| "-1".into()),
                        f(3, -1.0) as i64,
                        f(4, 0.0),
                        t.get(5).cloned().unwrap_or_default(),
                    ))
                }
                "MIDIPLINK" => midi = Some((i(1, 0), i(2, 0), i(3, 0), i(4, 0))),
                _ => m.extra.push(node.clone()),
            }
        }
        if let Some((scale, fx, param, offset, tail)) = link {
            let source = match (fx.as_str(), midi) {
                ("-100", Some((bus, channel, status, data1))) => LinkSource::Midi { bus, channel, status, data1 },
                _ => LinkSource::Fx { fx, param },
            };
            m.link = Some(ParamLink { scale, offset, source, tail });
            m.link_active = true;
        }
        Some(m)
    }

    /// The chunk as REAPER writes it: the LFO, audio control and link settings only while each
    /// is on.
    pub fn to_chunk(&self) -> Chunk {
        let mut c = Chunk { header: String::new(), children: Vec::new() };
        c.header = format!(
            "PROGRAMENV {} {} {}",
            crate::rpp::quote(&self.param),
            u8::from(self.disabled),
            quote_always(&self.label)
        );
        let n = num;
        c.line(["PARAMBASE".into(), n(self.base)]);
        c.line(["LFO".into(), u8::from(self.lfo.active).to_string()]);
        c.line(["LFOWT".into(), n(self.lfo.strength), self.lfo.direction.to_string()]);
        c.line(["AUDIOCTL".into(), u8::from(self.acs.active).to_string()]);
        c.line(["AUDIOCTLWT".into(), n(self.acs.strength), self.acs.direction.to_string()]);
        if self.lfo.active {
            let l = &self.lfo;
            c.line(["LFOSHAPE".into(), l.shape.to_string()]);
            c.line([
                "LFOSYNC".into(),
                u8::from(l.tempo_sync).to_string(),
                l.sync_flag.to_string(),
                u8::from(l.free).to_string(),
            ]);
            c.line(["LFOSPEED".into(), n(l.speed), n(l.phase)]);
        }
        if self.acs.active {
            let a = &self.acs;
            c.line(["CHAN".into(), a.channel.to_string()]);
            c.line(["STEREO".into(), u8::from(a.stereo).to_string()]);
            c.line(["RMS".into(), n(a.attack_ms), n(a.release_ms)]);
            c.line(["DBLO".into(), n(a.db_low)]);
            c.line(["DBHI".into(), n(a.db_high)]);
            c.line(["X2".into(), n(a.x2)]);
            c.line(["Y2".into(), n(a.y2)]);
        }
        if let (Some(link), true) = (&self.link, self.link_active) {
            let (fx, param) = match &link.source {
                LinkSource::Fx { fx, param } => (fx.clone(), *param),
                LinkSource::Midi { .. } => ("-100".into(), -1),
            };
            c.children.push(Node::Line(format!(
                "PLINK {} {} {} {} {}",
                n(link.scale),
                fx,
                param,
                n(link.offset),
                quote_always(&link.tail)
            )));
            if let LinkSource::Midi { bus, channel, status, data1 } = link.source {
                c.line([
                    "MIDIPLINK".into(),
                    bus.to_string(),
                    channel.to_string(),
                    status.to_string(),
                    data1.to_string(),
                ]);
            }
        }
        c.children.extend(self.extra.iter().cloned());
        c
    }
}

/// REAPER's number formatting in these chunks: up to six decimals, trailing zeros dropped.
fn num(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        if v.is_sign_negative() && v != 0.0 { "-0".into() } else { "0".into() }
    } else {
        s.into()
    }
}

/// The index in `3` or `3:_Name`.
pub fn param_index(spec: &str) -> Option<usize> {
    spec.split_once(':').map_or(spec, |(i, _)| i).parse().ok()
}

/// How a learned MIDI controller moves its parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LearnMode {
    Absolute,
    /// 127 = -1, 1 = +1.
    Relative1,
    /// 63 = -1, 65 = +1.
    Relative2,
    /// 65 = -1, 1 = +1.
    Relative3,
    /// Any value above zero toggles.
    Toggle,
    Other(u8),
}

impl LearnMode {
    pub const CHOICES: [LearnMode; 5] =
        [Self::Absolute, Self::Relative1, Self::Relative2, Self::Relative3, Self::Toggle];

    pub fn from_code(c: u8) -> Self {
        match c {
            0 => Self::Absolute,
            1 => Self::Relative1,
            2 => Self::Relative2,
            3 => Self::Relative3,
            4 => Self::Toggle,
            n => Self::Other(n),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Absolute => 0,
            Self::Relative1 => 1,
            Self::Relative2 => 2,
            Self::Relative3 => 3,
            Self::Toggle => 4,
            Self::Other(n) => n,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Absolute => "Absolute",
            Self::Relative1 => "Relative 1 (127=-1, 1=+1)",
            Self::Relative2 => "Relative 2 (63=-1, 65=+1)",
            Self::Relative3 => "Relative 3 (65=-1, 1=+1)",
            Self::Toggle => "Toggle (>0=toggle)",
            Self::Other(_) => "Other",
        }
    }

    /// The step a relative message asks for, in controller units, or `None` in other modes.
    pub fn relative_step(self, value: u8) -> Option<i32> {
        let v = value as i32;
        match self {
            Self::Relative1 => Some(if v >= 64 { v - 128 } else { v }),
            Self::Relative2 => Some(v - 64),
            Self::Relative3 => Some(if v >= 64 { -(v - 64) } else { v }),
            _ => None,
        }
    }
}

/// `PARMLEARN` flags.
pub const LEARN_SELECTED_TRACK: u32 = 1;
pub const LEARN_SOFT_TAKEOVER: u32 = 2;
pub const LEARN_FOCUSED_FX: u32 = 4;
/// Written as an `LFOLEARN` line: the message restarts the parameter's LFO.
pub const LEARN_LFO_RETRIGGER: u32 = 8;
pub const LEARN_VISIBLE_FX: u32 = 16;

/// A `PARMLEARN` (or `LFOLEARN`) line: a MIDI message or OSC address bound to a parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamLearn {
    pub param: String,
    /// Status byte, first data byte and mode packed as REAPER stores them:
    /// `status | data1 << 8 | mode << 16`; 0 when only OSC is learned.
    pub midi: u32,
    pub flags: u32,
    pub osc: String,
}

impl ParamLearn {
    pub fn midi(param: impl Into<String>, status: u8, data1: u8, mode: LearnMode) -> Self {
        Self {
            param: param.into(),
            midi: status as u32 | (data1 as u32) << 8 | (mode.code() as u32) << 16,
            flags: 0,
            osc: String::new(),
        }
    }

    pub fn index(&self) -> Option<usize> {
        param_index(&self.param)
    }

    pub fn status(&self) -> u8 {
        (self.midi & 0xff) as u8
    }

    pub fn data1(&self) -> u8 {
        (self.midi >> 8 & 0xff) as u8
    }

    pub fn mode(&self) -> LearnMode {
        LearnMode::from_code((self.midi >> 16 & 0xff) as u8)
    }

    pub fn set_mode(&mut self, mode: LearnMode) {
        self.midi = self.midi & 0xffff | (mode.code() as u32) << 16;
    }

    /// Whether a channel message matches the learned one: the same type and channel, and for
    /// notes and controllers the same number. Note-offs match a learned note.
    pub fn matches(&self, msg: [u8; 3]) -> bool {
        let s = self.status();
        if s < 0x80 {
            return false;
        }
        let kind = msg[0] & 0xf0;
        let kind = if kind == 0x80 { 0x90 } else { kind };
        if kind != s & 0xf0 || msg[0] & 0x0f != s & 0x0f {
            return false;
        }
        matches!(kind, 0xe0 | 0xd0 | 0xc0) || msg[1] == self.data1()
    }

    pub fn from_line(line: &str) -> Option<Self> {
        let key = line_key(line);
        if key != "PARMLEARN" && key != "LFOLEARN" {
            return None;
        }
        let t = tokenize(line);
        let n = |i: usize| t.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) as u32;
        let mut flags = n(3);
        if key == "LFOLEARN" {
            flags |= LEARN_LFO_RETRIGGER;
        }
        Some(Self { param: t.get(1)?.clone(), midi: n(2), flags, osc: t.get(4).cloned().unwrap_or_default() })
    }

    pub fn to_line(&self) -> String {
        let key = if self.flags & LEARN_LFO_RETRIGGER != 0 { "LFOLEARN" } else { "PARMLEARN" };
        let osc = if self.osc.is_empty() { String::new() } else { crate::rpp::quote(&self.osc) };
        format!("{key} {} {} {} {osc}", crate::rpp::quote(&self.param), self.midi, self.flags).trim_end().to_owned()
    }
}

fn is_learn(node: &Node) -> bool {
    matches!(node, Node::Line(l) if matches!(line_key(l), "PARMLEARN" | "LFOLEARN"))
}

fn is_modulation(node: &Node) -> bool {
    matches!(node, Node::Chunk(c) if c.name() == "PROGRAMENV")
}

/// The slot's parameter modulations.
pub fn modulations(slot: &FxSlot) -> Vec<ParamModulation> {
    slot.raw
        .iter()
        .filter_map(|n| match n {
            Node::Chunk(c) => ParamModulation::from_chunk(c),
            Node::Line(_) => None,
        })
        .collect()
}

pub fn modulation(slot: &FxSlot, index: usize) -> Option<ParamModulation> {
    modulations(slot).into_iter().find(|m| m.index() == Some(index))
}

/// Stores `m` in place of the slot's modulation of the same parameter, or after the last one.
pub fn set_modulation(slot: &mut FxSlot, m: &ParamModulation) {
    let node = Node::Chunk(m.to_chunk());
    let same = |n: &Node| matches!(n, Node::Chunk(c) if ParamModulation::from_chunk(c).is_some_and(|o| o.index() == m.index()));
    if let Some(existing) = slot.raw.iter_mut().find(|n| same(n)) {
        *existing = node;
        return;
    }
    let at = match slot.raw.iter().rposition(is_modulation) {
        Some(i) => i + 1,
        None => slot
            .raw
            .iter()
            .position(|n| match n {
                Node::Chunk(c) => c.name() == "PARMENV",
                Node::Line(l) => matches!(line_key(l), "PARMLEARN" | "LFOLEARN" | "WAK"),
            })
            .unwrap_or(slot.raw.len()),
    };
    slot.raw.insert(at, node);
}

pub fn remove_modulation(slot: &mut FxSlot, index: usize) -> bool {
    let before = slot.raw.len();
    slot.raw.retain(
        |n| !matches!(n, Node::Chunk(c) if ParamModulation::from_chunk(c).is_some_and(|m| m.index() == Some(index))),
    );
    slot.raw.len() != before
}

pub fn learns(slot: &FxSlot) -> Vec<ParamLearn> {
    slot.raw
        .iter()
        .filter_map(|n| match n {
            Node::Line(l) => ParamLearn::from_line(l),
            Node::Chunk(_) => None,
        })
        .collect()
}

pub fn learn(slot: &FxSlot, index: usize) -> Option<ParamLearn> {
    learns(slot).into_iter().find(|l| l.index() == Some(index))
}

/// Stores `l` in place of the slot's binding of the same parameter, or before `WAK`.
pub fn set_learn(slot: &mut FxSlot, l: &ParamLearn) {
    let node = Node::Line(l.to_line());
    let same =
        |n: &Node| matches!(n, Node::Line(s) if ParamLearn::from_line(s).is_some_and(|o| o.index() == l.index()));
    if let Some(existing) = slot.raw.iter_mut().find(|n| same(n)) {
        *existing = node;
        return;
    }
    let at = match slot.raw.iter().rposition(is_learn) {
        Some(i) => i + 1,
        None => {
            slot.raw.iter().position(|n| matches!(n, Node::Line(s) if line_key(s) == "WAK")).unwrap_or(slot.raw.len())
        }
    };
    slot.raw.insert(at, node);
}

pub fn remove_learn(slot: &mut FxSlot, index: usize) -> bool {
    let before = slot.raw.len();
    slot.raw
        .retain(|n| !matches!(n, Node::Line(s) if ParamLearn::from_line(s).is_some_and(|l| l.index() == Some(index))));
    slot.raw.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::{parse, write};

    const CHAIN: &str = "<FXCHAIN\nBYPASS 0 0 0\n<JS scythe/dcparam \"\"\n0.5 0 0 - - -\n>\nFLOATPOS 0 0 0 0\nFXID {X}\n<PROGRAMENV 0 0 \"Value / Param DC\"\nPARAMBASE 0.25\nLFO 1\nLFOWT 0.4 -1\nAUDIOCTL 1\nAUDIOCTLWT 0.3 0\nLFOSHAPE 2\nLFOSYNC 1 0 1\nLFOSPEED 0.437225 0.3\nCHAN 2\nSTEREO 1\nRMS 10 1000\nDBLO -60\nDBHI -6\nX2 0.2\nY2 0.8\nPLINK 0.5 1:1 2 0.1 \"\"\n>\n<PROGRAMENV 2:_Freq 1 \"Third / Param DC\"\nPARAMBASE 50\nLFO 0\nLFOWT 1 1\nAUDIOCTL 0\nAUDIOCTLWT 1 1\nPLINK 1 -100 -1 0 \"\"\nMIDIPLINK 0 2 176 11\n>\nPARMLEARN 0 133040 3 \nLFOLEARN 1 1968 8 \nPARMLEARN 2 0 0 /foo/bar\nWAK 0 0\n>\n";

    fn slot() -> FxSlot {
        let chunk = parse(CHAIN).unwrap();
        FxSlot { name: "x".into(), enabled: true, raw: chunk.children }
    }

    #[test]
    fn reads_every_programenv_field() {
        let mods = modulations(&slot());
        assert_eq!(mods.len(), 2);
        let m = &mods[0];
        assert_eq!((m.index(), m.base, m.disabled), (Some(0), 0.25, false));
        assert_eq!((m.lfo.active, m.lfo.strength, m.lfo.direction, m.lfo.shape), (true, 0.4, -1, 2));
        assert_eq!((m.lfo.tempo_sync, m.lfo.free, m.lfo.speed, m.lfo.phase), (true, true, 0.437225, 0.3));
        assert_eq!((m.acs.channel, m.acs.stereo, m.acs.attack_ms, m.acs.release_ms), (2, true, 10.0, 1000.0));
        assert_eq!((m.acs.db_low, m.acs.db_high, m.acs.x2, m.acs.y2), (-60.0, -6.0, 0.2, 0.8));
        let link = m.link.as_ref().unwrap();
        assert_eq!(link.source, LinkSource::Fx { fx: "1:1".into(), param: 2 });
        assert_eq!(link.source_fx(0), Some(1));
        let m = &mods[1];
        assert_eq!((m.param.as_str(), m.disabled, m.base), ("2:_Freq", true, 50.0));
        assert_eq!(m.link.as_ref().unwrap().source, LinkSource::Midi { bus: 0, channel: 2, status: 176, data1: 11 });
    }

    #[test]
    fn writes_chunks_back_as_read() {
        let original = parse(CHAIN).unwrap();
        let mut s = slot();
        for m in modulations(&s) {
            set_modulation(&mut s, &m);
        }
        for l in learns(&s) {
            set_learn(&mut s, &l);
        }
        let rebuilt = Chunk { header: original.header.clone(), children: s.raw };
        assert_eq!(write(&rebuilt), write(&original));
    }

    #[test]
    fn reads_learn_lines() {
        let l = learns(&slot());
        assert_eq!(l.len(), 3);
        assert_eq!((l[0].status(), l[0].data1(), l[0].mode(), l[0].flags), (0xb0, 7, LearnMode::Relative2, 3));
        assert_eq!(l[1].flags & LEARN_LFO_RETRIGGER, LEARN_LFO_RETRIGGER);
        assert_eq!((l[2].midi, l[2].osc.as_str()), (0, "/foo/bar"));
        assert!(l[0].matches([0xb0, 7, 3]) && !l[0].matches([0xb1, 7, 3]) && !l[0].matches([0xb0, 8, 3]));
    }

    #[test]
    fn inserts_new_entries_where_reaper_puts_them() {
        let mut s = slot();
        remove_modulation(&mut s, 0);
        remove_modulation(&mut s, 2);
        for i in 0..3 {
            remove_learn(&mut s, i);
        }
        let mut m = ParamModulation::new("1", "Second / Param DC", 0.0);
        m.lfo.active = true;
        set_modulation(&mut s, &m);
        set_learn(&mut s, &ParamLearn::midi("1", 0xb0, 1, LearnMode::Absolute));
        let keys: Vec<String> = s
            .raw
            .iter()
            .map(|n| match n {
                Node::Line(l) => line_key(l).to_owned(),
                Node::Chunk(c) => c.name().to_owned(),
            })
            .collect();
        assert_eq!(keys, ["BYPASS", "JS", "FLOATPOS", "FXID", "PROGRAMENV", "PARMLEARN", "WAK"]);
    }

    #[test]
    fn lfo_rates_match_reaper() {
        let mut l = Lfo { speed: 0.124573, ..Lfo::default() };
        assert!((l.hz() - 1.0000036).abs() < 1e-6);
        l.speed = 0.5;
        assert_eq!(l.period_qn(), 4.0);
        l.speed = 1.0;
        assert_eq!(l.period_qn(), 0.25);
        assert!((Lfo::speed_for_period_qn(2.125) - 0.75).abs() < 1e-12);
    }

    #[test]
    fn audio_control_curve() {
        let a = AudioControl { x2: 0.2, y2: 0.8, ..AudioControl::default() };
        // Measured from REAPER: -6.02 dB on a -24..0 range with the control point at (0.2, 0.8).
        assert!((a.shape(0.5) - 0.9163).abs() < 1e-4);
        assert!((AudioControl::default().shape(0.5) - 0.74914).abs() < 1e-5);
    }
}
