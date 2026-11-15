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
use std::path::Path;

use scythe_core::fx_chain::ParamEnvelope;
use scythe_core::model::{SCYTHE_FORMAT, Writer};
use scythe_core::rpp::ProjectIoError;
use scythe_core::{Envelope, FxSlot, Project};

use crate::clap_host::ParamInfo;
use crate::fx::{FxOwner, FxRegistry, slot_key};
use crate::scythe_eq::params;

/// The first format revision that stores CLAP automation in the parameters' units.
const UNITS_FORMAT: u32 = 2;

/// The parameters a hosted plugin lists, for the slot at a chain key (as `FxRegistry` keys its
/// instances): its running instance's, or with the last argument set those of an instance made
/// for the question; `None` when they cannot be known.
pub type SlotParams<'a> = dyn FnMut(&FxOwner, &str, &FxSlot, bool) -> Option<Vec<ParamInfo>> + 'a;

/// The parameters a hosted plugin lists, from an instance made for the question.
pub type MadeParams<'a> = dyn FnMut(&FxSlot) -> Option<Vec<ParamInfo>> + 'a;

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
        (self.min, self.max) != (0.0, 1.0) && self.min.is_finite() && self.max.is_finite() && self.max > self.min
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
    upgrade_project_report(project, params).changed
}

/// What converting a project did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Upgraded {
    pub changed: bool,
    /// The effects of a project from an older Scythe whose modulation baselines stay as stored,
    /// as no instance of their CLAP plugin could tell its ranges. Once the project is saved, only
    /// a 0..1 envelope beside them would still show they are old.
    pub unconverted: Vec<String>,
}

/// [`upgrade_project`], telling what could not be converted.
pub fn upgrade_project_report(project: &mut Project, params: &mut SlotParams) -> Upgraded {
    let all = match project.writer {
        Writer::Other => return Upgraded::default(),
        Writer::Scythe(format) => format < UNITS_FORMAT,
        Writer::Unknown => false,
    };
    let mut pass = Pass { all, make: all, params, pooled: Vec::new(), made: HashMap::new(), unconverted: Vec::new() };
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
    Upgraded { changed, unconverted: pass.unconverted }
}

/// Converts the slots of tracks or effects a file brings into a project (an FX chain, a track
/// template). Such a file carries no marker, so as in a project without one only a 0..1 envelope
/// on a parameter with another range shows that an older Scythe wrote the slot; the pools its
/// items play belong to the project it came from and stay. `params` lists a hosted plugin's
/// parameters from an instance made for the question, as none runs for the slot yet.
pub fn upgrade_tracks(tracks: &mut [scythe_core::Track], params: &mut MadeParams) -> bool {
    let mut changed = false;
    for track in tracks {
        changed |= upgrade_slots(&mut track.fx, params);
        let mut input = track.input_fx();
        if upgrade_slots(&mut input, params) {
            track.set_input_fx(&input);
            changed = true;
        }
        for take in track.items.iter_mut().flat_map(|i| &mut i.takes) {
            let mut fx = take.fx();
            if upgrade_slots(&mut fx, params) {
                take.set_fx(&fx);
                changed = true;
            }
        }
    }
    changed
}

/// [`upgrade_tracks`] for effects alone.
pub fn upgrade_slots(slots: &mut [FxSlot], params: &mut MadeParams) -> bool {
    let mut ask = |_: &FxOwner, _: &str, slot: &FxSlot, create: bool| if create { params(slot) } else { None };
    let mut pass = Pass {
        all: false,
        make: true,
        params: &mut ask,
        pooled: Vec::new(),
        made: HashMap::new(),
        unconverted: Vec::new(),
    };
    pass.chain(&FxOwner::Master, slots, "")
}

/// [`upgrade_project`] for a project rendered without a session, with `fx`, a registry of its
/// own that runs none of its plugins.
pub fn upgrade_read(project: &mut Project, fx: &FxRegistry) -> bool {
    upgrade_project(project, &mut |owner, key, slot, create| fx.slot_params(owner, key, slot, create))
}

/// Reads a project file to render it without a session, converted as opening it converts it.
pub fn read_project(path: &Path) -> Result<Project, ProjectIoError> {
    let mut project = scythe_core::rpp::read_project(path)?;
    // Parameter ranges do not depend on the rate.
    upgrade_read(&mut project, &FxRegistry::new(48_000));
    Ok(project)
}

struct Pass<'p, 'a> {
    /// Whether every baseline is old, not only those of slots with an old envelope.
    all: bool,
    /// Whether a plugin no instance runs may be made to learn its parameters.
    make: bool,
    params: &'p mut SlotParams<'a>,
    /// Pools played by the envelopes converted, with how their values move.
    pooled: Vec<(i64, Units)>,
    /// The parameters of plugins made for the question, by CLAP id: one instance each per pass,
    /// also when making it failed.
    made: HashMap<String, Option<Vec<ParamInfo>>>,
    /// See [`Upgraded::unconverted`].
    unconverted: Vec<String>,
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
        let Some(units) = self.units(owner, key, slot) else {
            if self.all && !modulations.is_empty() && crate::fx::is_clap(slot) {
                self.unconverted.push(slot.name.clone());
            }
            return false;
        };
        let at = |index: Option<usize>| index.and_then(|i| units.get(i).copied().flatten()).filter(Units::moves);
        let mut changed = false;
        for mut pe in old {
            let Some(u) = at(pe.param()) else { continue };
            for p in &mut pe.envelope.points {
                p.value = u.old_to_units(p.value);
            }
            pe.default = (u.min + u.max) / 2.0;
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
    /// CLAP plugin's; `None` for other formats, whose envelopes REAPER keeps on 0..1 too. A hosted
    /// plugin no instance runs is made only in an old file or one brought in, once per plugin id.
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
        let list = match (self.params)(owner, key, slot, false) {
            Some(list) => list,
            None if self.make => {
                let id = crate::fx::clap_id(slot)?;
                if !self.made.contains_key(&id) {
                    let made = (self.params)(owner, key, slot, true);
                    self.made.insert(id.clone(), made);
                }
                self.made[&id].clone()?
            }
            None => return None,
        };
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

    fn hosted(_: &FxOwner, _: &str, slot: &FxSlot, _: bool) -> Option<Vec<ParamInfo>> {
        (!crate::scythe_eq::is_slot(slot)).then(gain_params)
    }

    #[test]
    fn an_old_clap_envelope_and_baseline_move_to_the_parameters_units() {
        let mut p = project(Writer::Scythe(1), clap_slot(&format!("{OLD_BOOST}{OLD_BASE}")));
        assert!(upgrade_project(&mut p, &mut hosted));
        let slot = &p.tracks[0].fx[0];
        let env = slot.parameter_envelope("1").unwrap();
        assert_eq!((env.spec.as_str(), env.min, env.max, env.default), ("1:9", 0.0, 4.0, 2.0));
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
        assert!(!upgrade_project(&mut unknown, &mut |_, _, _, _| None), "the plugin's ranges are unknown");
        assert_eq!(unknown.tracks[0].fx[0], both);
    }

    /// A plugin without a running instance (offline, an inactive take) is made only for an old
    /// file, once per plugin; elsewhere a 0..1 envelope, which stays 0..1 on a 0..1 parameter
    /// for good, would make one on every load.
    #[test]
    fn plugins_are_made_only_for_old_files_and_once_per_plugin() {
        let slot = |guid: &str| {
            let mut s = clap_slot(&format!("{OLD_BOOST}{OLD_BASE}"));
            s.raw[2] = Node::Line(format!("FXID {guid}"));
            s
        };
        let two = |writer| {
            let mut p = project(writer, slot("{A}"));
            p.tracks[0].fx.push(slot("{B}"));
            p
        };
        for writer in [Writer::Other, Writer::Unknown, Writer::Scythe(SCYTHE_FORMAT)] {
            let mut p = two(writer);
            let mut made = 0;
            upgrade_project(&mut p, &mut |_, _, _, create| {
                made += usize::from(create);
                None
            });
            assert_eq!(made, 0, "{writer:?}");
        }

        let mut p = two(Writer::Scythe(1));
        let mut asked = Vec::new();
        assert!(upgrade_project(&mut p, &mut |_, key, _, create| {
            asked.push((key.to_owned(), create));
            create.then(gain_params)
        }));
        assert_eq!(asked, [("{A}".into(), false), ("{A}".into(), true), ("{B}".into(), false)]);
        assert!(p.tracks[0].fx.iter().all(|s| values(s, "1") == [1.0, 3.0] && base(s, 1) == 2.0));
    }

    /// A parameter whose range is not finite has no units to move to.
    #[test]
    fn an_infinite_range_is_left_alone() {
        let mut p = project(Writer::Scythe(1), clap_slot(&format!("{OLD_BOOST}{OLD_BASE}")));
        let before = p.tracks[0].fx[0].clone();
        assert!(!upgrade_project(&mut p, &mut |_, _, _, _| {
            let mut list = gain_params();
            list[1].max = f64::INFINITY;
            Some(list)
        }));
        assert_eq!(p.tracks[0].fx[0], before);
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
        assert!(!upgrade_project(&mut p, &mut |_, _, _, _| Some(gain_params())));
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
        assert!(upgrade_project(&mut p, &mut |owner, key, slot, create| {
            asked.push((owner.clone(), key.to_owned(), create));
            hosted(owner, key, slot, create)
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
                (FxOwner::Master, "{5}".to_owned(), false),
                (FxOwner::Track(id), format!("{container_key}/{{5}}"), false),
                (FxOwner::Input(id), "{5}".to_owned(), false),
                (FxOwner::Take(item, 0), "{5}".to_owned(), false),
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

        assert!(upgrade_project(&mut p, &mut |_, _, _, _| None));
        let slot = &p.tracks[0].fx[0];
        assert_eq!(values(slot, "2"), [params::from_normalized(freq, 0.5)]);
        assert!((base(slot, 2) - params::from_normalized(freq, 0.25)).abs() < 1e-4, "stored as REAPER rounds it");
        let pool = |id: i64| p.automation_pools.iter().find(|q| q.id == id).unwrap().points.clone();
        let own: Vec<f64> = pool(1).iter().map(|q| q.value).collect();
        assert_eq!(own, [linear(params::from_normalized(freq, 0.5)), 1.0]);
        let kept: Vec<f64> = pool(2).iter().map(|q| q.value).collect();
        assert_eq!(kept, [0.5, 1.0], "the volume envelope plays it too");

        let upgraded = p.clone();
        assert!(!upgrade_project(&mut p, &mut |_, _, _, _| None), "a second load changes nothing");
        assert_eq!(p, upgraded);
    }

    /// An old file whose hosted plugin cannot be made is marked current all the same, so the
    /// effects whose baselines stayed as stored are told; a file without a marker is converted
    /// on a later load still, wherever a 0..1 envelope shows it.
    #[test]
    fn baselines_left_for_a_missing_plugin_are_told() {
        let missing = |_: &FxOwner, _: &str, _: &FxSlot, _: bool| None;
        for slot in [clap_slot(OLD_BASE), clap_slot(&format!("{OLD_BOOST}{OLD_BASE}"))] {
            let mut p = project(Writer::Scythe(1), slot.clone());
            let report = upgrade_project_report(&mut p, &mut missing.clone());
            assert_eq!(report, Upgraded { changed: false, unconverted: vec![slot.name.clone()] });
            assert_eq!(p.tracks[0].fx[0], slot);

            let mut p = project(Writer::Unknown, slot.clone());
            assert_eq!(upgrade_project_report(&mut p, &mut missing.clone()), Upgraded::default());
        }
        let mut p = project(Writer::Scythe(1), clap_slot(OLD_BOOST));
        assert_eq!(upgrade_project_report(&mut p, &mut missing.clone()), Upgraded::default(), "no baseline to lose");
        let mut p = project(Writer::Scythe(1), clap_slot(OLD_BASE));
        assert!(upgrade_project_report(&mut p, &mut hosted).unconverted.is_empty());
    }

    /// Effects a chain file or a track template brings in convert as a file without a marker
    /// does, a hosted plugin's parameters coming from an instance made for the question.
    #[test]
    fn brought_in_effects_convert_where_an_old_envelope_shows() {
        let freq = params::automatable().nth(2).unwrap();
        let old_eq = eq_slot("<PARMENV 2 0 1 0.25\nACT 1 -1\nPT 0 0.5 0\n>\n");
        let base_only = clap_slot(OLD_BASE);
        let mut slots = vec![clap_slot(&format!("{OLD_BOOST}{OLD_BASE}")), base_only.clone(), old_eq.clone()];
        let mut made = 0;
        assert!(upgrade_slots(&mut slots, &mut |_| {
            made += 1;
            Some(gain_params())
        }));
        assert_eq!(values(&slots[0], "1"), [1.0, 3.0]);
        assert_eq!(base(&slots[0], 1), 2.0);
        assert_eq!(slots[1], base_only, "nothing shows the baseline is old");
        assert_eq!(values(&slots[2], "2"), [params::from_normalized(freq, 0.5)]);
        assert_eq!(made, 1);

        let mut take = Take::from_file("vocal.wav".into());
        take.set_fx(std::slice::from_ref(&old_eq));
        let mut track = Track::new("Vocals");
        track.fx.push(old_eq.clone());
        track.set_input_fx(std::slice::from_ref(&old_eq));
        track.items.push(Item::new(0.0, 1.0, take));
        let mut tracks = vec![track];
        assert!(upgrade_tracks(&mut tracks, &mut |_| None));
        let converted = |slot: &FxSlot| values(slot, "2") == [params::from_normalized(freq, 0.5)];
        assert!(converted(&tracks[0].fx[0]));
        assert!(converted(&tracks[0].input_fx()[0]));
        assert!(converted(&tracks[0].items[0].takes[0].fx()[0]));
        assert!(!upgrade_tracks(&mut tracks, &mut |_| None), "a second time changes nothing");
    }
}

#[cfg(test)]
mod read_tests {
    /// Renders that read a file convert it as opening it does; REAPER's files stay as they are.
    #[test]
    fn a_file_read_to_render_is_converted_by_its_writer() {
        let mut project = scythe_core::Project::default();
        let mut track = scythe_core::Track::new("Vox");
        let mut slot = crate::scythe_eq::slot(None);
        let mut m = scythe_core::modulation::ParamModulation::new("2", "Freq", 0.25);
        m.lfo.active = true;
        scythe_core::modulation::set_modulation(&mut slot, &m);
        track.fx.push(slot);
        project.tracks.push(track);
        let text = scythe_core::rpp::project_to_rpp(&project);
        let header = text.lines().next().unwrap().to_owned();
        let dir = std::env::temp_dir().join(format!("scythe-read-upgrade-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = |version: &str| {
            let path = dir.join(format!("{}.RPP", version.len()));
            std::fs::write(&path, text.replacen(&header, &format!("<REAPER_PROJECT 0.1 \"{version}\" 0"), 1)).unwrap();
            let p = super::read_project(&path).unwrap();
            scythe_core::modulation::modulation(&p.tracks[0].fx[0], 2).unwrap().base
        };
        assert!((base("scythe-0.1.0") - 74.0).abs() < 0.1, "{}", base("scythe-0.1.0"));
        assert_eq!(base("7.73/linux-x86_64"), 0.25);
        std::fs::remove_dir_all(&dir).ok();
    }
}
