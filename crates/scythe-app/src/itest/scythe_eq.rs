//! ScyEQ-Pro's editor: adding it from the FX browser, adding bands with the mouse, moving
//! one and several, removing them with the keyboard, undo, and the instance list.

use egui::accesskit::Role;
use egui::vec2;
use scythe_engine::scythe_eq::params::{self as p, band};
use scythe_engine::scythe_eq::slot_values;

use super::TestApp;

fn values(t: &TestApp) -> Vec<f64> {
    slot_values(&t.track("Drums").fx[0]).expect("a ScyEQ-Pro state")
}

fn used(v: &[f64]) -> Vec<usize> {
    (0..p::BANDS).filter(|b| v[p::band_param(*b, band::USED) as usize] >= 0.5).collect()
}

#[test]
fn scythe_eq_editor_adds_moves_and_removes_bands() {
    let mut t = TestApp::live("scythe_eq");
    t.click("Track Drums");
    t.click("Add plugin");
    t.click("ScyEQ-Pro");
    assert_eq!(t.track("Drums").fx.len(), 1);
    assert!(scythe_engine::scythe_eq::is_slot(&t.track("Drums").fx[0]));
    t.click_role(Role::Button, "Edit");
    let drums = t.track("Drums").id;
    assert!(crate::ui::fx_editor::is_open(t.app(), Some(drums), 0), "the editor opened");
    let graph = t.rect("ScyEQ-Pro graph");

    // Double-clicks add a bell where they land, and a cut near the ends. A pause before each
    // keeps egui from counting it with the click before as a triple click.
    for at in [
        graph.center() + vec2(-120.0, -40.0),
        graph.center() + vec2(160.0, 30.0),
        graph.left_center() + vec2(12.0, 0.0),
    ] {
        t.frames(12);
        t.double_click_at(at);
    }
    let v = values(&t);
    assert_eq!(used(&v), vec![0, 1, 2], "three bands");
    assert!(v[p::band_param(0, band::GAIN) as usize] > 1.0, "above the 0 dB line is a boost");
    assert!(v[p::band_param(1, band::GAIN) as usize] < -1.0);
    assert_eq!(v[p::band_param(2, band::SHAPE) as usize], 3.0, "a low cut at the bottom end");

    // Dragging a band up raises its gain; undo puts it back.
    let before = values(&t);
    let h = t.rect("Band 1");
    t.drag(h.center(), h.center() + vec2(0.0, -30.0));
    let after = values(&t);
    assert!(after[p::band_param(0, band::GAIN) as usize] > before[p::band_param(0, band::GAIN) as usize] + 1.0);
    t.key_mod(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(values(&t)[p::band_param(0, band::GAIN) as usize], before[p::band_param(0, band::GAIN) as usize]);

    // Two selected bands move together.
    let (b1, b2) = (t.rect("Band 1"), t.rect("Band 2"));
    t.click_at(b1.center());
    t.frames(12);
    t.h.event(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
    t.click_at_mod(b2.center(), egui::Modifiers::COMMAND);
    t.h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    t.frames(12);
    let f = |v: &[f64], b: usize| v[p::band_param(b, band::FREQ) as usize];
    let before = values(&t);
    t.drag(b1.center(), b1.center() + vec2(60.0, 0.0));
    let after = values(&t);
    let ratio = |b| f(&after, b) / f(&before, b);
    assert!(ratio(0) > 1.1 && (ratio(0) - ratio(1)).abs() < 0.02, "{} {}", ratio(0), ratio(1));

    // Delete removes the selection.
    t.h.event(egui::Event::PointerMoved(graph.center()));
    t.frames(2);
    t.key(egui::Key::Delete);
    assert_eq!(used(&values(&t)), vec![2]);

    // The instance list shows the EQ and its band.
    t.click("Instances");
    assert!(t.app().view.eq_instances);
    assert!(t.has("Drums band 3"), "{:#?}", t.labels());
}
