//! A ScyEQ-Pro that REAPER keeps as the VST3 plugin runs in Scythe as the built-in EQ and is
//! saved back in the same `<VST` block.

use std::path::PathBuf;

use scythe_core::rpp::{Node, project_from_rpp, project_to_rpp};
use scythe_core::{FxSlot, Project, Track};
use scythe_engine::fx::{FxRegistry, FxStatus, vst3_slot};
use scythe_engine::scythe_eq::filters::Shape;
use scythe_engine::scythe_eq::params::{self, band, band_param};
use scythe_engine::scythe_eq::{self, ScytheEq};
use scythe_engine::vst2::ReaperVstState;
use scythe_engine::vst3_host;

const RATE: u32 = 48_000;

/// The nearest plugin library above the test: beside it in `deps` with cargo's old build layout,
/// otherwise the one `cargo build -p scyeq-plugin` put in the profile directory.
fn plugin_library() -> PathBuf {
    let file = format!("{}scyeq_plugin{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    let exe = std::env::current_exe().unwrap();
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join(&file))
        .find(|library| library.exists())
        .unwrap_or_else(|| panic!("{file} not built next to or above {}: cargo build -p scyeq-plugin", exe.display()))
}

fn state() -> Vec<u8> {
    let mut v = params::defaults();
    for (b, shape, freq, gain) in [(0, Shape::LowShelf, 120.0, -2.5), (1, Shape::Bell, 2400.0, 3.0)] {
        v[band_param(b, band::USED) as usize] = 1.0;
        v[band_param(b, band::SHAPE) as usize] = shape.index() as f64;
        v[band_param(b, band::FREQ) as usize] = freq;
        v[band_param(b, band::GAIN) as usize] = gain;
    }
    params::encode(&v, &[], params::VERSION)
}

fn installed() -> Vec<vst3_host::Vst3Info> {
    vst3_host::Vst3Module::load(&plugin_library()).unwrap().classes("ScyEQ-Pro.vst3")
}

/// A registry that hosts the ScyEQ-Pro VST3 plugin when `hosting`, else runs the built-in EQ.
fn registry(hosting: bool) -> FxRegistry {
    let mut fx = FxRegistry::new(RATE);
    fx.set_vst3_plugins(if hosting { installed() } else { Vec::new() });
    fx.host_scyeq_plugin = hosting;
    fx
}

/// `slot` with the component state `state` and no controller state, as REAPER stores it.
fn with_state(mut slot: FxSlot, state: &[u8]) -> FxSlot {
    let Some(Node::Chunk(chunk)) = slot.raw.get_mut(1) else { panic!("no plugin chunk") };
    let lines: Vec<String> = chunk
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Line(l) => Some(l.clone()),
            Node::Chunk(_) => None,
        })
        .collect();
    let lines = ReaperVstState::reencode(&lines, &vst3_host::reaper_state(state, &[])).unwrap();
    chunk.children = lines.into_iter().map(Node::Line).collect();
    slot
}

/// Runs `project`'s first slot in `fx` and saves the project, with how the slot ran.
fn save(fx: &mut FxRegistry, mut project: Project) -> (String, FxStatus) {
    let track = &project.tracks[0];
    assert_eq!(fx.chain(Some(track.id), &track.fx).len(), 1, "{:?}", fx.status(Some(track.id), &track.fx[0], 0));
    let status = fx.status(Some(track.id), &track.fx[0], 0).unwrap().clone();
    fx.store_states(&mut project);
    (project_to_rpp(&project), status)
}

/// A project with a VST3 ScyEQ-Pro holding `state()`, saved by Scythe hosting the built plugin:
/// the layout REAPER writes for it.
fn reaper_project() -> String {
    let info = installed().remove(0);
    let mut project = Project::default();
    let mut track = Track::new("Vocals");
    track.fx.push(with_state(vst3_slot(&info), &state()));
    project.tracks.push(track);
    let (text, status) = save(&mut registry(true), project);
    assert!(matches!(status, FxStatus::Hosted(_)), "{status:?}");
    text
}

/// The `<VST` block of `text`.
fn vst_block(text: &str) -> String {
    let lines: Vec<&str> = text.lines().skip_while(|l| !l.trim_start().starts_with("<VST")).collect();
    let end = lines.iter().position(|l| l.trim() == ">").expect("a closed <VST block");
    lines[..=end].join("\n")
}

fn vst_state(block: &str) -> ReaperVstState {
    let lines: Vec<String> = block.lines().skip(1).map(|l| l.trim().to_owned()).filter(|l| l != ">").collect();
    ReaperVstState::decode(&lines).unwrap()
}

fn running_values(fx: &FxRegistry, project: &Project) -> Vec<f64> {
    let track = &project.tracks[0];
    let inst = fx.instance(Some(track.id), &track.fx[0], 0).unwrap();
    inst.with(|e| e.as_any_mut().and_then(|a| a.downcast_mut::<ScytheEq>()).map(|eq| eq.values().to_vec()))
        .flatten()
        .expect("the built-in EQ")
}

#[test]
fn vst3_slot_runs_built_in() {
    let text = reaper_project();
    assert!(text.contains("<VST \"VST3: ScyEQ-Pro (Scythe)\" ScyEQ-Pro.vst3 0 \"\" "), "{text}");
    let project = project_from_rpp(&text).unwrap();
    let slot = &project.tracks[0].fx[0];
    assert!(scythe_eq::is_slot(slot));
    assert_eq!(scythe_engine::stock::shown_name(&slot.name), scythe_eq::NAME);
    let stored = params::decode(&state()).unwrap().values;
    assert_eq!(scythe_eq::slot_values(slot), Some(stored.clone()));

    let mut fx = registry(false);
    let track = &project.tracks[0];
    assert_eq!(fx.chain(Some(track.id), &track.fx).len(), 1);
    assert_eq!(fx.status(Some(track.id), slot, 0), Some(&FxStatus::Native(scythe_eq::STATUS)));
    assert_eq!(running_values(&fx, &project), stored);
}

#[test]
fn vst3_slot_round_trips() {
    let text = reaper_project();
    let (saved, status) = save(&mut registry(false), project_from_rpp(&text).unwrap());
    assert_eq!(status, FxStatus::Native(scythe_eq::STATUS));
    assert_eq!(vst_block(&saved), vst_block(&text));
    assert_eq!(saved, text);
}

#[test]
fn edited_vst3_slot_saves_vst3() {
    let text = reaper_project();
    let project = project_from_rpp(&text).unwrap();
    let mut fx = registry(false);
    let track = &project.tracks[0];
    fx.chain(Some(track.id), &track.fx);
    let gain = band_param(1, band::GAIN);
    fx.instance(Some(track.id), &track.fx[0], 0).unwrap().with(|e| e.set_param(gain, -4.5));
    let mut project = project;
    fx.store_states(&mut project);
    let saved = project_to_rpp(&project);
    assert!(!saved.contains("<CLAP"), "{saved}");

    let (before, after) = (vst_block(&text), vst_block(&saved));
    assert_eq!(before.lines().next(), after.lines().next(), "the same plugin line");
    assert_ne!(before, after);
    let (old, new) = (vst_state(&before), vst_state(&after));
    assert_eq!(
        (old.unique_id, &old.inputs, &old.outputs, old.words),
        (new.unique_id, &new.inputs, &new.outputs, new.words)
    );
    assert_eq!(old.program, new.program);
    let (_, old_controller) = vst3_host::split_reaper_state(&old.chunk).unwrap();
    let (component, controller) = vst3_host::split_reaper_state(&new.chunk).unwrap();
    assert_eq!(controller, old_controller);
    let values = params::decode(component).unwrap().values;
    assert_eq!(values[gain as usize], -4.5);
    assert_eq!(scythe_eq::slot_values(&project.tracks[0].fx[0]), Some(values));

    let reloaded = project_from_rpp(&saved).unwrap();
    let (hosted, status) = save(&mut registry(true), reloaded);
    assert!(matches!(status, FxStatus::Hosted(_)), "{status:?}");
    assert_eq!(vst_block(&hosted), after, "the plugin reads the edited block and saves it unchanged");
}

/// A project REAPER 7.73 saved with the ScyEQ-Pro VST3 on a track, set through ReaScript by
/// `fixtures/reaper-scyeq-vst3.lua`.
const REAPER_WROTE: &str = include_str!("fixtures/reaper-scyeq-vst3.RPP");

#[test]
fn a_vst3_slot_reaper_wrote_runs_built_in_and_saves_unchanged() {
    let project = project_from_rpp(REAPER_WROTE).unwrap();
    let slot = &project.tracks[0].fx[0];
    assert!(scythe_eq::is_slot(slot));
    let values = scythe_eq::slot_values(slot).expect("the plugin's state");
    let set = [
        (band_param(0, band::USED), 1.0),
        (band_param(0, band::SHAPE), 2.0 / 9.0),
        (band_param(0, band::FREQ), 0.25),
        (band_param(0, band::GAIN), 0.4),
        (band_param(1, band::USED), 1.0),
        (band_param(1, band::FREQ), 0.6),
        (band_param(1, band::GAIN), 0.7),
        (band_param(1, band::Q), 0.5),
        (params::GLOBAL + params::global::OUTPUT, 0.55),
    ];
    // REAPER hands the normalised values over in single precision.
    for (id, v) in params::defaults().into_iter().enumerate() {
        let id = id as u32;
        let expected = set.iter().find(|s| s.0 == id).map_or(v, |&(_, n)| params::from_normalized(id, n as f32 as f64));
        let stored = values[id as usize];
        assert!(
            (stored - expected).abs() <= 1e-9 * expected.abs().max(1.0),
            "{}: {stored} against {expected}",
            params::name(id)
        );
    }
    assert_eq!(Shape::from_index(values[band_param(0, band::SHAPE) as usize] as usize), Shape::HighShelf);

    let mut fx = registry(false);
    let (saved, status) = save(&mut fx, project.clone());
    assert_eq!(status, FxStatus::Native(scythe_eq::STATUS));
    assert_eq!(running_values(&fx, &project), values);
    assert_eq!(vst_block(&saved), vst_block(REAPER_WROTE).replace('\r', ""), "nothing edited, nothing rewritten");
}
