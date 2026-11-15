//! Converts what older Scythe versions stored in other units than REAPER, when a project opens.
//!
//! REAPER keeps a CLAP plugin's parameter envelopes (`PARMENV <index>:<id> <min> <max>`) and
//! modulation baselines (`PARAMBASE`) in the parameter's own units, and pooled automation items
//! normalised linearly over the envelope's range. Scythe before format revision 2 stored hosted
//! CLAP plugins' envelopes and baselines on 0..1 (linear over the range), and ScyEQ-Pro's
//! envelopes, baselines and pooled items log-normalised as its VST3 plugin maps them.
//!
//! The project's writer decides: a file REAPER wrote is never converted, and one an older Scythe
//! wrote is converted wholly. Elsewhere (no marker, or a newer Scythe that could not convert a
//! slot because its plugin was missing) only a 0..1 envelope on a parameter with another range
//! shows a slot is old, which REAPER never writes; its baselines are then converted with it.

use std::collections::HashMap;

use scythe_core::fx_chain::ParamEnvelope;
use scythe_core::model::{SCYTHE_FORMAT, Writer};
use scythe_core::{Envelope, FxSlot, Project};

use crate::clap_host::ParamInfo;
use crate::fx::{FxOwner, slot_key};
use crate::scythe_eq::params;

/// The first format revision that stores CLAP automation in the parameters' units.
const UNITS_FORMAT: u32 = 2;

/// The parameters a hosted plugin lists, for the slot at a chain key (as `FxRegistry` keys its
/// instances); `None` when they cannot be known.
pub type SlotParams<'a> = dyn FnMut(&FxOwner, &str, &FxSlot) -> Option<Vec<ParamInfo>> + 'a;

/// A parameter's range in its own units, and how the value an older Scythe stored maps into it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Units {
    id: u32,
    min: f64,
    max: f64,
    /// A ScyEQ-Pro parameter, stored as `params::to_normalized` maps it.
    scyeq: bool,
}

impl Units {
    fn moves(&self) -> bool {
        (self.min, self.max) != (0.0, 1.0) && self.max > self.min
    }

    fn old_to_units(&self, v: f64) -> f64 {
        if self.scyeq {
            params::from_normalized(self.id, v)
        } else {
            self.min + v.clamp(0.0, 1.0) * (self.max - self.min)
        }
    }

    /// An old pooled value, normalised over the range as pools store it.
    fn pooled(&self, n: f64) -> f64 {
        (self.old_to_units(n) - self.min) / (self.max - self.min)
    }
}

/// Converts the CLAP parameter envelopes, modulation baselines and pooled automation items of
/// every chain that an older Scythe stored in other units than REAPER. `params` lists a hosted
/// plugin's parameters; it is asked only for slots that may need them. Returns whether anything
/// changed. A project from an older Scythe is marked as current afterwards, so converting it
/// again changes nothing.
pub fn upgrade_project(project: &mut Project, params: &mut SlotParams) -> bool {
    let all = match project.writer {
        Writer::Other => return false,
        Writer::Scythe(format) => format < UNITS_FORMAT,
        Writer::Unknown => false,
    };
    let mut pass = Pass { all, params, pooled: Vec::new() };
    let mut changed = pass.chain(&FxOwner::Master, &mut project.master.fx, "");
    for track in &mut project.tracks {
        let id = track.id;
        changed |= pass.chain(&FxOwner::Track(id), &mut track.fx, "");
        let mut input = track.input_fx();
        if pass.chain(&FxOwner::Input(id), &mut input, "") {
            track.set_input_fx(&input);
            changed = true;
        }
        for item in &mut track.items {
            let item_id = item.id;
            for (k, take) in item.takes.iter_mut().enumerate() {
                let mut fx = take.fx();
                if pass.chain(&FxOwner::Take(item_id, k), &mut fx, "") {
                    take.set_fx(&fx);
                    changed = true;
                }
            }
        }
    }
    let pooled = std::mem::take(&mut pass.pooled);
    if upgrade_pools(project, &pooled) {
        project.resolve_automation_items();
        changed = true;
    }
    if all {
        project.writer = Writer::Scythe(SCYTHE_FORMAT);
    }
    changed
}

struct Pass<'p, 'a> {
    /// Whether every baseline is old, not only those of slots with an old envelope.
    all: bool,
    params: &'p mut SlotParams<'a>,
    /// Pools played by the envelopes converted, with how their values move.
    pooled: Vec<(i64, Units)>,
}

impl Pass<'_, '_> {
    fn chain(&mut self, owner: &FxOwner, slots: &mut [FxSlot], prefix: &str) -> bool {
        let mut changed = false;
        for (i, slot) in slots.iter_mut().enumerate() {
            let key = format!("{prefix}{}", slot_key(slot, i));
            if slot.is_container() {
                let mut children = slot.container_children();
                if self.chain(owner, &mut children, &format!("{key}/")) {
                    slot.set_container_children(&children);
                    changed = true;
                }
            } else {
                changed |= self.slot(owner, &key, slot);
            }
        }
        changed
    }

    fn slot(&mut self, owner: &FxOwner, key: &str, slot: &mut FxSlot) -> bool {
        let old: Vec<ParamEnvelope> =
            slot.parameter_envelopes().into_iter().filter(|e| (e.min, e.max) == (0.0, 1.0)).collect();
        let modulations = scythe_core::modulation::modulations(slot);
        if old.is_empty() && !(self.all && !modulations.is_empty()) {
            return false;
        }
        let Some(units) = self.units(owner, key, slot) else { return false };
        let at = |index: Option<usize>| index.and_then(|i| units.get(i).copied().flatten()).filter(Units::moves);
        let mut changed = false;
        for mut pe in old {
            let Some(u) = at(pe.param()) else { continue };
            for p in &mut pe.envelope.points {
                p.value = u.old_to_units(p.value);
            }
            pe.default = u.old_to_units(pe.default);
            (pe.min, pe.max) = (u.min, u.max);
            pe.envelope.range = Some((u.min, u.max));
            pe.spec = format!("{}:{}", pe.param().unwrap_or_default(), u.id);
            self.pooled.extend(pe.envelope.items.iter().map(|item| (item.pool, u)));
            slot.set_parameter_envelope(&pe);
            changed = true;
        }
        if self.all || changed {
            for mut m in modulations {
                let Some(u) = at(m.index()) else { continue };
                m.base = u.old_to_units(m.base);
                scythe_core::modulation::set_modulation(slot, &m);
                changed = true;
            }
        }
        changed
    }

    /// The units of each parameter of the slot's plugin, by index: ScyEQ-Pro's own, or a hosted
    /// CLAP plugin's; `None` for other formats, whose envelopes REAPER keeps on 0..1 too.
    fn units(&mut self, owner: &FxOwner, key: &str, slot: &FxSlot) -> Option<Vec<Option<Units>>> {
        if crate::scythe_eq::is_clap_slot(slot) {
            return Some(
                params::automatable()
                    .map(|id| params::spec_of(id).map(|s| Units { id, min: s.min, max: s.max, scyeq: true }))
                    .collect(),
            );
        }
        if crate::scythe_eq::is_slot(slot) || !crate::fx::is_clap(slot) {
            return None;
        }
        let list = (self.params)(owner, key, slot)?;
        Some(list.iter().map(|p| Some(Units { id: p.id, min: p.min, max: p.max, scyeq: false })).collect())
    }
}

/// Moves the pools whose every instance is on an envelope just converted, the same way; a pool
/// also played elsewhere keeps its values. Linear moves leave normalised values as they are.
fn upgrade_pools(project: &mut Project, pooled: &[(i64, Units)]) -> bool {
    let mut moves: HashMap<i64, (usize, Option<Units>)> = HashMap::new();
    for &(pool, u) in pooled {
        let entry = moves.entry(pool).or_insert((0, Some(u)));
        entry.0 += 1;
        if entry.1 != Some(u) {
            entry.1 = None;
        }
    }
    let uses = pool_uses(project);
    let mut changed = false;
    for pool in &mut project.automation_pools {
        let Some(&(count, Some(u))) = moves.get(&pool.id) else { continue };
        if !u.scyeq || uses.get(&pool.id) != Some(&count) {
            continue;
        }
        for p in &mut pool.points {
            p.value = u.pooled(p.value);
        }
        changed = true;
    }
    changed
}

/// How many automation items play each pool, over every envelope of the project.
fn pool_uses(project: &Project) -> HashMap<i64, usize> {
    let mut uses = HashMap::new();
    let mut count = |env: &Envelope| {
        for item in &env.items {
            *uses.entry(item.pool).or_insert(0) += 1;
        }
    };
    fn slots(chain: &[FxSlot], count: &mut dyn FnMut(&Envelope)) {
        for slot in chain {
            if slot.is_container() {
                slots(&slot.container_children(), count);
            }
            slot.parameter_envelopes().iter().for_each(|p| count(&p.envelope));
        }
    }
    for track in std::iter::once(&project.master).chain(&project.tracks) {
        track.envelopes.iter().for_each(&mut count);
        track.sends.iter().flat_map(|s| &s.envelopes).for_each(&mut count);
        slots(&track.fx, &mut count);
        slots(&track.input_fx(), &mut count);
        for take in track.items.iter().flat_map(|i| &i.takes) {
            take.envelopes.iter().for_each(&mut count);
            slots(&take.fx(), &mut count);
        }
    }
    uses
}

#[cfg(test)]
mod tests {
    use scythe_core::automation_item::{AutomationItem, AutomationPool};
    use scythe_core::model::{SCYTHE_FORMAT, Writer};
    use scythe_core::rpp::{Chunk, Node, parse};
    use scythe_core::{EnvPoint, Item, Take, Track};

    use super::*;
    use crate::scythe_eq::params;

    /// A hosted CLAP plugin listing a 0..1 gain (id 7) and a 0..4 boost (id 9).
    fn gain_params() -> Vec<ParamInfo> {
        let p = |id, name: &str, max| ParamInfo { id, name: name.into(), min: 0.0, max, default: 0.0, log: false };
        vec![p(7, "Gain", 1.0), p(9, "Boost", 4.0)]
    }

    fn with_children(mut slot: FxSlot, children: &str) -> FxSlot {
        let wak = slot.raw.pop().unwrap();
        slot.raw.extend(parse(&format!("<X\n{children}>\n")).unwrap().children);
        slot.raw.push(wak);
        slot
    }

    fn clap_slot(children: &str) -> FxSlot {
        let mut chunk = Chunk::new("CLAP", &["CLAP: Test Gain (Scythe)", "org.scythe.test-gain", ""]);
        chunk.line(["CFG", "4", "0", "0", ""]);
        let raw = vec![
            Node::Line("BYPASS 0 0 0".into()),
            Node::Chunk(chunk),
            Node::Line("FXID {5}".into()),
            Node::Line("WAK 0 0".into()),
        ];
        with_children(FxSlot { name: "CLAP: Test Gain (Scythe)".into(), enabled: true, raw }, children)
    }

    fn eq_slot(children: &str) -> FxSlot {
        with_children(crate::scythe_eq::slot(None), children)
    }

    const OLD_BOOST: &str = "<PARMENV 1 0 1 0.25\nACT 1 -1\nPT 0 0.25 0\nPT 1 0.75 0\n>\n";
    const OLD_BASE: &str = "<PROGRAMENV 1 0 \"Boost\"\nPARAMBASE 0.5\nLFO 1\nLFOWT 0.2 1\n>\n";

    fn project(writer: Writer, slot: FxSlot) -> Project {
        let mut track = Track::new("Gtr");
        track.fx.push(slot);
        Project { writer, tracks: vec![track], ..Project::default() }
    }

    fn base(slot: &FxSlot, index: usize) -> f64 {
        scythe_core::modulation::modulation(slot, index).unwrap().base
    }

    fn values(slot: &FxSlot, spec: &str) -> Vec<f64> {
        slot.parameter_envelope(spec).unwrap().envelope.points.iter().map(|p| p.value).collect()
    }

    fn hosted(_: &FxOwner, _: &str, slot: &FxSlot) -> Option<Vec<ParamInfo>> {
        (!crate::scythe_eq::is_slot(slot)).then(gain_params)
    }

    #[test]
    fn an_old_clap_envelope_and_baseline_move_to_the_parameters_units() {
        let mut p = project(Writer::Scythe(1), clap_slot(&format!("{OLD_BOOST}{OLD_BASE}")));
        assert!(upgrade_project(&mut p, &mut hosted));
        let slot = &p.tracks[0].fx[0];
        let env = slot.parameter_envelope("1").unwrap();
        assert_eq!((env.spec.as_str(), env.min, env.max, env.default), ("1:9", 0.0, 4.0, 1.0));
        assert_eq!(values(slot, "1"), [1.0, 3.0]);
        assert_eq!(base(slot, 1), 2.0);
        assert_eq!(p.writer, Writer::Scythe(SCYTHE_FORMAT), "converted once");

        let upgraded = p.clone();
        assert!(!upgrade_project(&mut p, &mut hosted), "a second load changes nothing");
        assert_eq!(p, upgraded);
    }

    /// The writer decides: a REAPER file is never converted, an old Scythe one is converted
    /// wholly, and one without a marker (or that a newer Scythe wrote, for slots whose plugin
    /// was missing the first time) only where a 0..1 envelope shows it.
    #[test]
    fn the_writer_decides_what_is_converted() {
        let base_only = clap_slot(OLD_BASE);
        let both = clap_slot(&format!("{OLD_BOOST}{OLD_BASE}"));

        for slot in [&base_only, &both] {
            let mut p = project(Writer::Other, slot.clone());
            assert!(!upgrade_project(&mut p, &mut hosted), "REAPER wrote it");
            assert_eq!(p.tracks[0].fx[0], *slot);
            assert_eq!(p.writer, Writer::Other);
        }

        let mut p = project(Writer::Scythe(1), base_only.clone());
        assert!(upgrade_project(&mut p, &mut hosted));
        assert_eq!(base(&p.tracks[0].fx[0], 1), 2.0, "an old file's baselines are all old");

        for writer in [Writer::Unknown, Writer::Scythe(SCYTHE_FORMAT)] {
            let mut p = project(writer, base_only.clone());
            assert!(!upgrade_project(&mut p, &mut hosted), "nothing shows the baseline is old");
            let mut p = project(writer, both.clone());
            assert!(upgrade_project(&mut p, &mut hosted));
            assert_eq!(values(&p.tracks[0].fx[0], "1"), [1.0, 3.0]);
            assert_eq!(base(&p.tracks[0].fx[0], 1), 2.0, "the 0..1 envelope shows the slot is old");
        }

        let mut unknown = project(Writer::Scythe(1), both.clone());
        assert!(!upgrade_project(&mut unknown, &mut |_, _, _| None), "the plugin's ranges are unknown");
        assert_eq!(unknown.tracks[0].fx[0], both);
    }

    /// A 0..1 parameter reads the same either way, and other plugin formats keep REAPER's
    /// normalised values.
    #[test]
    fn zero_to_one_parameters_and_other_formats_stay() {
        let gain =
            clap_slot("<PARMENV 0 0 1 0.5\nACT 1 -1\nPT 0 0.25 0\n>\n<PROGRAMENV 0 0 \"\"\nPARAMBASE 0.5\nLFO 1\n>\n");
        let mut p = project(Writer::Scythe(1), gain.clone());
        upgrade_project(&mut p, &mut hosted);
        assert_eq!(values(&p.tracks[0].fx[0], "0"), [0.25]);
        assert_eq!(base(&p.tracks[0].fx[0], 0), 0.5);

        let mut vst = clap_slot(&format!("{OLD_BOOST}{OLD_BASE}"));
        for n in &mut vst.raw {
            if let Node::Chunk(c) = n
                && c.name() == "CLAP"
            {
                c.header = "VST \"VST: Gain\" gain.so 0 \"\" 1234".into();
            }
        }
        let mut p = project(Writer::Scythe(1), vst.clone());
        assert!(!upgrade_project(&mut p, &mut |_, _, _| Some(gain_params())));
        assert_eq!(p.tracks[0].fx[0], vst);
    }

    /// Hosted CLAP slots in every chain: master, take and input effects, and containers, each
    /// asked for by its owner and key.
    #[test]
    fn old_clap_slots_are_converted_in_every_chain() {
        let old = clap_slot(&format!("{OLD_BOOST}{OLD_BASE}"));
        let mut container = FxSlot::container();
        container.set_container_children(std::slice::from_ref(&old));
        let mut take = Take::from_file("vocal.wav".into());
        take.set_fx(std::slice::from_ref(&old));
        let mut track = Track::new("Vocals");
        track.set_input_fx(std::slice::from_ref(&old));
        track.fx.push(container);
        track.items.push(Item::new(0.0, 1.0, take));
        let mut p = Project { writer: Writer::Scythe(1), tracks: vec![track], ..Project::default() };
        p.master.fx.push(old.clone());
        let (id, item) = (p.tracks[0].id, p.tracks[0].items[0].id);

        let mut asked = Vec::new();
        assert!(upgrade_project(&mut p, &mut |owner, key, slot| {
            asked.push((owner.clone(), key.to_owned()));
            hosted(owner, key, slot)
        }));
        let converted = |slot: &FxSlot| values(slot, "1") == [1.0, 3.0] && base(slot, 1) == 2.0;
        let track = &p.tracks[0];
        assert!(converted(&p.master.fx[0]));
        assert!(converted(&track.input_fx()[0]));
        assert!(converted(&track.items[0].takes[0].fx()[0]));
        assert!(converted(&track.fx[0].container_children()[0]));
        let container_key = crate::fx::slot_key(&track.fx[0], 0);
        assert_eq!(
            asked,
            [
                (FxOwner::Master, "{5}".to_owned()),
                (FxOwner::Track(id), format!("{container_key}/{{5}}")),
                (FxOwner::Input(id), "{5}".to_owned()),
                (FxOwner::Take(item, 0), "{5}".to_owned()),
            ]
        );
    }

    /// ScyEQ-Pro's baselines were log-normalised as its envelopes were, and its pooled items
    /// are normalised over the envelope's range, so they move to linear with them: a pool only
    /// when every instance is on an envelope moving the same way.
    #[test]
    fn old_scyeq_baselines_and_pooled_items_move_with_its_envelopes() {
        let freq = params::automatable().nth(2).unwrap();
        let linear = |v: f64| {
            let s = params::spec_of(freq).unwrap();
            (v - s.min) / (s.max - s.min)
        };
        let pooled = |pool: i64| format!("POOLEDENVINST {pool} 0 1 0 1 0 0.5 1 0 0 0 0 0 0 0.012 0 1\n");
        let slot = eq_slot(&format!(
            "<PARMENV 2 0 1 0.25\nACT 1 -1\n{}{}PT 0 0.5 0\n>\n<PROGRAMENV 2 0 \"Freq\"\nPARAMBASE 0.25\nLFO 1\n>\n",
            pooled(1),
            pooled(2)
        ));
        let mut p = project(Writer::Scythe(1), slot);
        let mut shared = Envelope::new(scythe_core::EnvelopeKind::Volume);
        shared.items.push(AutomationItem::new(2, 0.0, 1.0));
        p.tracks[0].envelopes.push(shared);
        for id in [1, 2] {
            let mut pool = AutomationPool::new(id, 4.0);
            pool.points = vec![EnvPoint::new(0.0, 0.5), EnvPoint::new(1.0, 1.0)];
            p.automation_pools.push(pool);
        }

        assert!(upgrade_project(&mut p, &mut |_, _, _| None));
        let slot = &p.tracks[0].fx[0];
        assert_eq!(values(slot, "2"), [params::from_normalized(freq, 0.5)]);
        assert!((base(slot, 2) - params::from_normalized(freq, 0.25)).abs() < 1e-4, "stored as REAPER rounds it");
        let pool = |id: i64| p.automation_pools.iter().find(|q| q.id == id).unwrap().points.clone();
        let own: Vec<f64> = pool(1).iter().map(|q| q.value).collect();
        assert_eq!(own, [linear(params::from_normalized(freq, 0.5)), 1.0]);
        let kept: Vec<f64> = pool(2).iter().map(|q| q.value).collect();
        assert_eq!(kept, [0.5, 1.0], "the volume envelope plays it too");

        let upgraded = p.clone();
        assert!(!upgrade_project(&mut p, &mut |_, _, _| None), "a second load changes nothing");
        assert_eq!(p, upgraded);
    }

    use scythe_core::Envelope;
}
