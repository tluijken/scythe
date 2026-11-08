//! Runs a stock JSFX from the installed REAPER. Skipped when no REAPER `Effects` folder is
//! found.

use scythe_core::rpp::{project_from_rpp, project_to_rpp};
use scythe_engine::fx::{FxRegistry, FxStatus, ProcessContext};

#[test]
fn soft_clipper_runs_and_keeps_its_sliders() {
    if scythe_engine::jsfx::find("schwa/soft_clipper", &scythe_engine::jsfx::search_paths()).is_none() {
        eprintln!("no REAPER Effects folder; skipping");
        return;
    }
    let mut project = project_from_rpp(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    <FXCHAIN\n      BYPASS 0 0 0\n      <JS schwa/soft_clipper \"\"\n        1 -1 - - -\n      >\n      FXID {00000000-0000-0000-0000-000000000002}\n      WAK 0 0\n    >\n  >\n>\n",
    )
    .unwrap();
    let id = project.tracks[0].id;
    let mut fx = FxRegistry::new(48000);
    let chain = fx.chain(Some(id), &project.tracks[0].fx);
    assert!(matches!(fx.status(Some(id), &project.tracks[0].fx[0], 0), Some(FxStatus::Native(_))));
    let params = chain[0].with(|e| e.params()).unwrap();
    assert_eq!(params[0].name, "Boost (dB)");
    assert_eq!(chain[0].with(|e| e.param(1)).flatten().map(|p| p.0), Some(-1.0));

    // A loud sine comes out under the -1 dB brickwall.
    let ctx = ProcessContext {
        sample_rate: 48000,
        position: 0,
        playing: true,
        notes: None,
        live: &[],
        live_channel: None,
        bpm: 120.0,
    };
    let (mut l, mut r): (Vec<f32>, Vec<f32>) = (0..4800).map(|i| ((i as f32 * 0.05).sin(), 0.0)).unzip();
    chain[0].process(&mut l, &mut r, &ctx);
    let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak <= 10f32.powf(-1.0 / 20.0) + 1e-4 && peak > 0.5, "{peak}");

    chain[0].with(|e| e.set_param(0, 3.0));
    fx.store_states(&mut project);
    let text = project_to_rpp(&project);
    assert!(text.contains("<JS schwa/soft_clipper \"\"\n        3 -1 - -"), "{text}");
}
