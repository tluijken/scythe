//! Converts what older Scythe versions stored in other units than REAPER, when a project opens.
//!
//! REAPER keeps a CLAP plugin's parameter envelopes (`PARMENV <index>:<id> <min> <max>`) and
//! modulation baselines (`PARAMBASE`) in the parameter's own units. Scythe before format
//! revision 2 stored hosted CLAP plugins' envelopes and baselines on 0..1 (linear over the range).
//!
//! The project's writer decides: a file REAPER wrote is never converted, and one an older Scythe
//! wrote is converted wholly. Elsewhere (no marker, or a newer Scythe that could not convert a
//! slot because its plugin was missing) only a 0..1 envelope on a parameter with another range
//! shows a slot is old, which REAPER never writes; its baselines are then converted with it.

use scythe_core::fx_chain::ParamEnvelope;
use scythe_core::model::{SCYTHE_FORMAT, Writer};
use scythe_core::{FxSlot, Project};

use crate::clap_host::ParamInfo;
use crate::fx::{FxOwner, slot_key};

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
}

impl Units {
    fn moves(&self) -> bool {
        (self.min, self.max) != (0.0, 1.0) && self.max > self.min
    }

    fn old_to_units(&self, v: f64) -> f64 {
        self.min + v.clamp(0.0, 1.0) * (self.max - self.min)
    }
}

/// Converts the CLAP parameter envelopes and modulation baselines of every chain that an older Scythe stored in other units than REAPER. `params` lists a hosted
/// plugin's parameters; it is asked only for slots that may need them. Returns whether anything
/// changed. A project from an older Scythe is marked as current afterwards, so converting it
/// again changes nothing.
pub fn upgrade_project(project: &mut Project, params: &mut SlotParams) -> bool {
    let all = match project.writer {
        Writer::Other => return false,
        Writer::Scythe(format) => format < UNITS_FORMAT,
        Writer::Unknown => false,
    };
    let mut pass = Pass { all, params };
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
    if all {
        project.writer = Writer::Scythe(SCYTHE_FORMAT);
    }
    changed
}

struct Pass<'p, 'a> {
    /// Whether every baseline is old, not only those of slots with an old envelope.
    all: bool,
    params: &'p mut SlotParams<'a>,
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

    /// The units of each parameter of a hosted CLAP plugin, by index; `None` for other formats,
    /// whose envelopes REAPER keeps on 0..1 too, and ScyEQ-Pro (`scythe_eq::upgrade_project`).
    fn units(&mut self, owner: &FxOwner, key: &str, slot: &FxSlot) -> Option<Vec<Option<Units>>> {
        if crate::scythe_eq::is_slot(slot) || !crate::fx::is_clap(slot) {
            return None;
        }
        let list = (self.params)(owner, key, slot)?;
        Some(list.iter().map(|p| Some(Units { id: p.id, min: p.min, max: p.max })).collect())
    }
}

#[cfg(test)]
mod tests {
    use scythe_core::model::{SCYTHE_FORMAT, Writer};
    use scythe_core::rpp::{Chunk, Node, parse};
    use scythe_core::{Item, Take, Track};

    use super::*;

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
}
