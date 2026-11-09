//! The per-slot settings REAPER keeps around a plugin in an FX chain: wet/dry, offline,
//! parallel processing and containers. Slots keep their lines verbatim (`FxSlot::raw`), so
//! these read and edit those lines in place and projects round-trip untouched.

use crate::model::{FxSlot, Take, Track};
use crate::rpp::{Chunk, Node, join_tokens, line_key, tokenize};

/// How a slot joins the one before it (`PARALLEL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Parallel {
    /// After the previous effect, on its output.
    #[default]
    Serial,
    /// On the same input as the previous effect, the outputs summed; the previous effect's
    /// MIDI output goes on.
    Audio,
    /// As `Audio`, and its MIDI output is merged with the previous effect's.
    MergeMidi,
}

impl Parallel {
    pub fn from_code(v: i64) -> Self {
        match v {
            1 => Self::Audio,
            2 => Self::MergeMidi,
            _ => Self::Serial,
        }
    }

    pub fn code(self) -> i64 {
        match self {
            Self::Serial => 0,
            Self::Audio => 1,
            Self::MergeMidi => 2,
        }
    }
}

/// A container's channel layout (`CONTAINER_CFG`): its own channels and how many input and
/// output pins connect it to the chain around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerConfig {
    pub channels: u32,
    pub inputs: u32,
    pub outputs: u32,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self { channels: 2, inputs: 2, outputs: 2 }
    }
}

/// A container parameter that stands for a child's (`CONTAINER_PARM <id> <child> <param>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerParam {
    /// Stable id envelopes refer to (`PARMENV <index>:<id>`).
    pub id: u32,
    /// The child within the container, and its parameter (for a child container, one of its
    /// own container parameters).
    pub child: usize,
    pub param: usize,
}

/// Splits the body of an FX chain (or a container) into its slots: each starts with a
/// `BYPASS` line, followed by the plugin chunk and the lines that belong to it up to the next
/// `BYPASS`. Returns the slots and the lines before the first one. A container's
/// `CONTAINER_PARM` lines, which follow its last child, stay out of the slots.
pub fn split_chain(children: &[Node]) -> (Vec<FxSlot>, Vec<Node>) {
    let mut prelude = Vec::new();
    let mut fx: Vec<FxSlot> = Vec::new();
    for node in children {
        if let Node::Line(l) = node {
            let key = line_key(l);
            if key == "BYPASS" {
                let t = tokenize(l);
                let bypassed = t.get(1).is_some_and(|v| v != "0");
                fx.push(FxSlot { name: String::new(), enabled: !bypassed, raw: vec![node.clone()] });
                continue;
            }
            if key == "CONTAINER_PARM" {
                continue;
            }
        }
        match fx.last_mut() {
            Some(slot) => {
                if let Node::Chunk(plugin) = node
                    && slot.name.is_empty()
                {
                    slot.name = plugin.args().into_iter().next().unwrap_or_else(|| plugin.name().to_owned());
                }
                slot.raw.push(node.clone());
            }
            None => prelude.push(node.clone()),
        }
    }
    (fx, prelude)
}

/// The slots back as chain lines, with each slot's bypass state written into its `BYPASS`.
pub fn join_chain(slots: &[FxSlot]) -> Vec<Node> {
    let mut out = Vec::new();
    for slot in slots {
        for (i, node) in slot.raw.iter().enumerate() {
            match node {
                Node::Line(l) if i == 0 && line_key(l) == "BYPASS" => {
                    let mut toks = tokenize(l);
                    toks[1] = if slot.enabled { "0" } else { "1" }.to_owned();
                    out.push(Node::Line(join_tokens(toks)));
                }
                _ => out.push(node.clone()),
            }
        }
    }
    out
}

fn line_tokens(raw: &[Node], key: &str) -> Option<(usize, Vec<String>)> {
    raw.iter().enumerate().find_map(|(i, n)| match n {
        Node::Line(l) if line_key(l) == key => Some((i, tokenize(l))),
        _ => None,
    })
}

fn num(t: &[String], i: usize) -> Option<f64> {
    t.get(i).and_then(|v| v.parse().ok())
}

impl FxSlot {
    /// A new slot around `plugin`, enabled, with REAPER's per-slot lines.
    pub fn with_plugin(plugin: Chunk) -> Self {
        let name = plugin.args().into_iter().next().unwrap_or_else(|| plugin.name().to_owned());
        Self {
            name,
            enabled: true,
            raw: vec![
                Node::Line("BYPASS 0 0 0".into()),
                Node::Chunk(plugin),
                Node::Line(join_tokens(["FXID", &crate::model::new_guid()])),
                Node::Line("WAK 0 0".into()),
            ],
        }
    }

    /// The plugin's own chunk (`<VST`, `<JS`, `<CONTAINER`, …).
    pub fn plugin(&self) -> Option<&Chunk> {
        self.raw.iter().find_map(|n| match n {
            Node::Chunk(c) => Some(c),
            Node::Line(_) => None,
        })
    }

    fn plugin_mut(&mut self) -> Option<&mut Chunk> {
        self.raw.iter_mut().find_map(|n| match n {
            Node::Chunk(c) => Some(c),
            Node::Line(_) => None,
        })
    }

    fn bypass_field(&self, i: usize) -> bool {
        match self.raw.first() {
            Some(Node::Line(l)) if line_key(l) == "BYPASS" => tokenize(l).get(i).is_some_and(|v| v != "0"),
            _ => false,
        }
    }

    fn set_bypass_field(&mut self, i: usize, on: bool) {
        if !matches!(self.raw.first(), Some(Node::Line(l)) if line_key(l) == "BYPASS") {
            self.raw.insert(0, Node::Line("BYPASS 0 0 0".into()));
        }
        let Some(Node::Line(l)) = self.raw.first_mut() else { return };
        let mut t = tokenize(l);
        while t.len() <= i {
            t.push("0".into());
        }
        t[i] = if on { "1" } else { "0" }.into();
        *l = join_tokens(t);
    }

    /// Offline (`BYPASS` field 2): the plugin is unloaded and the slot passes its input on.
    pub fn offline(&self) -> bool {
        self.bypass_field(2)
    }

    pub fn set_offline(&mut self, offline: bool) {
        self.set_bypass_field(2, offline);
    }

    /// Whether the slot processes: neither bypassed nor offline.
    pub fn active(&self) -> bool {
        self.enabled && !self.offline()
    }

    /// The wet share (`WET` field 1, 0-1, linear) and whether the slot plays only the
    /// difference it makes (`WET` field 2, delta solo).
    pub fn wet(&self) -> (f64, bool) {
        match line_tokens(&self.raw, "WET") {
            Some((_, t)) => (num(&t, 1).unwrap_or(1.0).clamp(0.0, 1.0), num(&t, 2).is_some_and(|v| v != 0.0)),
            None => (1.0, false),
        }
    }

    /// Sets the wet share and delta solo. A slot that has no `WET` line gets one after the
    /// plugin chunk, with sample-accurate wet/dry as REAPER gives new slots.
    pub fn set_wet(&mut self, wet: f64, delta: bool) {
        let line = Node::Line(join_tokens(["WET", &fmt(wet.clamp(0.0, 1.0)), if delta { "1" } else { "0" }]));
        if let Some((i, _)) = line_tokens(&self.raw, "WET") {
            self.raw[i] = line;
            return;
        }
        let at = self.raw.iter().rposition(|n| matches!(n, Node::Chunk(_))).map_or(self.raw.len(), |i| i + 1);
        self.raw.insert(at, line);
        if line_tokens(&self.raw, "SAMPLE_ACCURATE_WETDRY").is_none() {
            self.raw.insert(at, Node::Line("SAMPLE_ACCURATE_WETDRY 1".into()));
        }
    }

    /// Whether wet/dry envelopes act per sample (`SAMPLE_ACCURATE_WETDRY 1`) rather than
    /// ramping once per block, a block late, as older projects do.
    pub fn sample_accurate_wet(&self) -> bool {
        line_tokens(&self.raw, "SAMPLE_ACCURATE_WETDRY").is_some_and(|(_, t)| num(&t, 1).is_some_and(|v| v != 0.0))
    }

    pub fn parallel(&self) -> Parallel {
        line_tokens(&self.raw, "PARALLEL")
            .map_or(Parallel::Serial, |(_, t)| Parallel::from_code(num(&t, 1).unwrap_or(0.0) as i64))
    }

    /// Sets how the slot joins the one before it; the `PARALLEL` line closes the slot.
    pub fn set_parallel(&mut self, parallel: Parallel) {
        let existing = line_tokens(&self.raw, "PARALLEL").map(|(i, _)| i);
        if let Some(i) = existing {
            self.raw.remove(i);
        }
        if parallel != Parallel::Serial {
            let line = Node::Line(join_tokens(["PARALLEL", &parallel.code().to_string()]));
            match existing {
                Some(i) => self.raw.insert(i, line),
                None => self.raw.push(line),
            }
        }
    }

    pub fn is_container(&self) -> bool {
        self.plugin().is_some_and(|c| c.name() == "CONTAINER")
    }

    /// A new, empty container slot.
    pub fn container() -> Self {
        let mut c = Chunk::new("CONTAINER", &["Container", ""]);
        c.line(["CONTAINER_CFG", "2", "2", "2", "0"]);
        c.push_chunk(Chunk::new("IN_PINS", &[]));
        c.push_chunk(Chunk::new("OUT_PINS", &[]));
        for l in ["SHOW 0", "LASTSEL 0", "DOCKED 0"] {
            c.children.push(Node::Line(l.into()));
        }
        Self::with_plugin(c)
    }

    pub fn container_config(&self) -> Option<ContainerConfig> {
        let c = self.plugin().filter(|c| c.name() == "CONTAINER")?;
        let t = c.find_line("CONTAINER_CFG").unwrap_or_default();
        let get = |i: usize| t.get(i).and_then(|v| v.parse::<u32>().ok()).filter(|&v| v > 0);
        let channels = get(0).unwrap_or(2);
        Some(ContainerConfig {
            channels,
            inputs: get(1).unwrap_or(2).min(channels),
            outputs: get(2).unwrap_or(2).min(channels),
        })
    }

    pub fn set_container_config(&mut self, cfg: ContainerConfig) {
        let Some(c) = self.plugin_mut().filter(|c| c.name() == "CONTAINER") else { return };
        let old = c.find_line("CONTAINER_CFG").unwrap_or_default();
        let flags = old.get(3).cloned().unwrap_or_else(|| "0".into());
        let line = Node::Line(join_tokens([
            "CONTAINER_CFG".to_owned(),
            cfg.channels.to_string(),
            cfg.inputs.to_string(),
            cfg.outputs.to_string(),
            flags,
        ]));
        match c.children.iter().position(|n| matches!(n, Node::Line(l) if line_key(l) == "CONTAINER_CFG")) {
            Some(i) => c.children[i] = line,
            None => c.children.insert(0, line),
        }
    }

    /// The effects inside a container, in order.
    pub fn container_children(&self) -> Vec<FxSlot> {
        match self.plugin().filter(|c| c.name() == "CONTAINER") {
            Some(c) => split_chain(&c.children).0,
            None => Vec::new(),
        }
    }

    /// Replaces a container's effects, keeping its own lines and parameter mappings (those of
    /// children that no longer exist are dropped by the caller).
    pub fn set_container_children(&mut self, children: &[FxSlot]) {
        let Some(c) = self.plugin_mut().filter(|c| c.name() == "CONTAINER") else { return };
        let (_, prelude) = split_chain(&c.children);
        let parms: Vec<Node> = c
            .children
            .iter()
            .filter(|n| matches!(n, Node::Line(l) if line_key(l) == "CONTAINER_PARM"))
            .cloned()
            .collect();
        c.children = prelude;
        c.children.extend(join_chain(children));
        c.children.extend(parms);
    }

    /// The child parameters a container exposes as its own, in its parameter order.
    pub fn container_params(&self) -> Vec<ContainerParam> {
        let Some(c) = self.plugin().filter(|c| c.name() == "CONTAINER") else { return Vec::new() };
        c.children
            .iter()
            .filter_map(|n| match n {
                Node::Line(l) if line_key(l) == "CONTAINER_PARM" => {
                    let t = tokenize(l);
                    let get = |i: usize| t.get(i).and_then(|v| v.parse::<usize>().ok());
                    Some(ContainerParam { id: get(1)? as u32, child: get(2)?, param: get(3)? })
                }
                _ => None,
            })
            .collect()
    }

    /// Exposes a child's parameter on the container; returns the mapping's id.
    pub fn add_container_param(&mut self, child: usize, param: usize) -> Option<u32> {
        let existing = self.container_params();
        if let Some(p) = existing.iter().find(|p| p.child == child && p.param == param) {
            return Some(p.id);
        }
        let id = existing.iter().map(|p| p.id).max().unwrap_or(0) + 1;
        let c = self.plugin_mut().filter(|c| c.name() == "CONTAINER")?;
        c.line(["CONTAINER_PARM", &id.to_string(), &child.to_string(), &param.to_string(), ""]);
        Some(id)
    }

    /// Input (`IN_PINS`) or output (`OUT_PINS`) pin map of a container or a plugin that keeps
    /// one in that form: per pin, a channel mask; pins not listed keep pin n on channel n.
    pub fn listed_pins(&self, output: bool) -> Option<Vec<u64>> {
        let c = self.plugin()?.chunks(if output { "OUT_PINS" } else { "IN_PINS" }).next()?;
        let mut masks: Vec<u64> = Vec::new();
        for node in &c.children {
            let Node::Line(l) = node else { continue };
            let t = tokenize(l);
            let Some(pin) = t.get(1).and_then(|v| v.parse::<usize>().ok()).filter(|&p| p < 64) else { continue };
            let mask =
                t[2..].iter().filter_map(|v| v.parse::<u32>().ok()).filter(|&c| c < 64).fold(0u64, |m, c| m | 1 << c);
            if masks.len() <= pin {
                masks.extend((masks.len()..=pin).map(|p| 1u64 << p));
            }
            masks[pin] = mask;
        }
        Some(masks)
    }

    /// Writes a container's pin map, listing only the pins that differ from pin n on channel n,
    /// as REAPER does.
    pub fn set_listed_pins(&mut self, output: bool, masks: &[u64]) {
        let Some(c) = self.plugin_mut() else { return };
        let name = if output { "OUT_PINS" } else { "IN_PINS" };
        let mut chunk = Chunk::new(name, &[]);
        for (pin, &mask) in masks.iter().enumerate() {
            if mask == 1u64 << pin {
                continue;
            }
            let mut t = vec!["PIN".to_owned(), pin.to_string()];
            t.extend((0..64).filter(|b| mask & (1u64 << b) != 0).map(|b: u32| b.to_string()));
            chunk.line(t);
        }
        match c.children.iter().position(|n| matches!(n, Node::Chunk(p) if p.name() == name)) {
            Some(i) => c.children[i] = Node::Chunk(chunk),
            None => c.push_chunk(chunk),
        }
    }
}

/// The slots of the chain chunk `name` among `nodes`, if there is one.
fn chain_in(nodes: &[Node], name: &str) -> Option<Vec<FxSlot>> {
    nodes.iter().find_map(|n| match n {
        Node::Chunk(c) if c.name() == name => Some(split_chain(&c.children).0),
        _ => None,
    })
}

/// Replaces the slots of chain chunk `name` among `nodes` (keeping its window lines), adding
/// the chunk when missing and dropping it when no slots are left.
fn set_chain_in(nodes: &mut Vec<Node>, name: &str, slots: &[FxSlot]) {
    let at = nodes.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == name));
    if slots.is_empty() {
        if let Some(i) = at {
            nodes.remove(i);
        }
        return;
    }
    let prelude = match at.map(|i| &nodes[i]) {
        Some(Node::Chunk(c)) => split_chain(&c.children).1,
        _ => ["SHOW 0", "LASTSEL 0", "DOCKED 0"].map(|l| Node::Line(l.into())).to_vec(),
    };
    let mut chunk = Chunk::new(name, &[]);
    chunk.children = prelude;
    chunk.children.extend(join_chain(slots));
    match at {
        Some(i) => nodes[i] = Node::Chunk(chunk),
        None => nodes.push(Node::Chunk(chunk)),
    }
}

impl Take {
    /// The take's own effects (`<TAKEFX`), which process it before it reaches the track.
    pub fn fx(&self) -> Vec<FxSlot> {
        chain_in(&self.extra, "TAKEFX").unwrap_or_default()
    }

    pub fn set_fx(&mut self, slots: &[FxSlot]) {
        set_chain_in(&mut self.extra, "TAKEFX", slots);
    }

    /// Channels the take's effects run on (`TAKEFX_NCH`); by default as many as the source
    /// has, at least two.
    pub fn fx_channels(&self) -> Option<u32> {
        line_tokens(&self.extra, "TAKEFX_NCH").and_then(|(_, t)| t.get(1)?.parse().ok()).filter(|&n| n >= 2)
    }

    pub fn set_fx_channels(&mut self, channels: Option<u32>) {
        let at = line_tokens(&self.extra, "TAKEFX_NCH").map(|(i, _)| i);
        match (at, channels) {
            (Some(i), None) => {
                self.extra.remove(i);
            }
            (Some(i), Some(n)) => self.extra[i] = Node::Line(format!("TAKEFX_NCH {n}")),
            (None, Some(n)) => self.extra.push(Node::Line(format!("TAKEFX_NCH {n}"))),
            (None, None) => {}
        }
    }
}

impl Track {
    /// The record input chain (`<FXCHAIN_REC`): effects on what the track records and
    /// monitors, ahead of the recording.
    pub fn input_fx(&self) -> Vec<FxSlot> {
        chain_in(&self.extra, "FXCHAIN_REC").unwrap_or_default()
    }

    pub fn set_input_fx(&mut self, slots: &[FxSlot]) {
        set_chain_in(&mut self.extra, "FXCHAIN_REC", slots);
    }
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.10}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(text: &str) -> Vec<FxSlot> {
        let root = crate::rpp::parse(&format!("<FXCHAIN\n{text}>\n")).unwrap();
        split_chain(&root.children).0
    }

    const CHAIN: &str = r#"BYPASS 0 1 0
<JS wp6/gain.jsfx ""
-6 - -
>
SAMPLE_ACCURATE_WETDRY 1
WET 0.3 1
WAK 0 0
BYPASS 0 0 0
<CONTAINER Container ""
CONTAINER_CFG 4 2 1 0
<IN_PINS
PIN 0 1
>
<OUT_PINS
>
SHOW 0
BYPASS 0 0 0
<JS a ""
>
BYPASS 1 0 0
<JS b ""
>
PARALLEL 2
CONTAINER_PARM 1 1 0 ""
>
WAK 0 0
PARALLEL 1
"#;

    #[test]
    fn reads_slot_settings() {
        let fx = chain(CHAIN);
        assert_eq!(fx.len(), 2);
        assert!(fx[0].offline() && fx[0].enabled && !fx[0].active());
        assert_eq!(fx[0].wet(), (0.3, true));
        assert!(fx[0].sample_accurate_wet());
        assert_eq!(fx[1].parallel(), Parallel::Audio);
        assert_eq!(fx[1].container_config(), Some(ContainerConfig { channels: 4, inputs: 2, outputs: 1 }));
        assert_eq!(fx[1].listed_pins(false), Some(vec![2]));
        assert_eq!(fx[1].listed_pins(true), Some(vec![]));
        let kids = fx[1].container_children();
        assert_eq!(kids.len(), 2);
        assert!(!kids[1].enabled);
        assert_eq!(kids[1].parallel(), Parallel::MergeMidi);
        assert_eq!(fx[1].container_params(), vec![ContainerParam { id: 1, child: 1, param: 0 }]);
    }

    #[test]
    fn edits_round_trip() {
        let mut fx = chain(CHAIN);
        assert_eq!(join_chain(&fx), crate::rpp::parse(&format!("<X\n{CHAIN}>\n")).unwrap().children);
        fx[0].set_wet(0.5, false);
        fx[0].set_offline(false);
        fx[1].set_parallel(Parallel::Serial);
        let mut kids = fx[1].container_children();
        kids[0].set_parallel(Parallel::Audio);
        fx[1].set_container_children(&kids);
        fx[1].set_listed_pins(true, &[1, 4]);
        let again = chain(&crate::rpp::write_body(&join_chain(&fx)));
        assert_eq!(again[0].wet(), (0.5, false));
        assert!(again[0].active());
        assert_eq!(again[1].parallel(), Parallel::Serial);
        assert_eq!(again[1].container_children()[0].parallel(), Parallel::Audio);
        assert_eq!(again[1].container_params().len(), 1);
        assert_eq!(again[1].listed_pins(true), Some(vec![1, 4]));
    }

    #[test]
    fn take_and_input_chains_live_in_their_chunks() {
        let mut take = crate::model::Take::from_file("a.wav".into());
        assert!(take.fx().is_empty());
        take.set_fx(&[FxSlot::with_plugin(Chunk::new("JS", &["x", ""]))]);
        take.set_fx_channels(Some(4));
        assert_eq!((take.fx().len(), take.fx_channels()), (1, Some(4)));
        take.set_fx(&[]);
        take.set_fx_channels(None);
        assert!(take.extra.is_empty());
        let mut t = crate::model::Track::new("t");
        t.set_input_fx(&[FxSlot::container()]);
        assert!(t.input_fx()[0].is_container());
    }

    #[test]
    fn new_wet_line_follows_the_plugin() {
        let mut s = FxSlot::with_plugin(Chunk::new("JS", &["x", ""]));
        s.set_wet(0.25, false);
        assert!(matches!(&s.raw[2], Node::Line(l) if l == "SAMPLE_ACCURATE_WETDRY 1"));
        assert!(matches!(&s.raw[3], Node::Line(l) if l == "WET 0.25 0"));
        let mut c = FxSlot::container();
        c.set_container_children(&[s]);
        assert_eq!(c.container_children().len(), 1);
        assert_eq!(c.add_container_param(0, 3), Some(1));
        assert_eq!(c.add_container_param(0, 3), Some(1));
        assert_eq!(c.container_params()[0].param, 3);
    }
}
