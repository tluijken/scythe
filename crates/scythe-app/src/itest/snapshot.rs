//! Pictures of views that draw the same every time, compared with a tolerance against the
//! ones under `tests/snapshots`. `UPDATE_SNAPSHOTS=1` writes them anew. Without a GPU adapter
//! (no Vulkan or GL driver), the comparison is skipped with a message.

use super::*;

fn renders(t: &mut TestApp) -> bool {
    match t.h.render() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("SKIPPED snapshot: no renderer here ({e})");
            false
        }
    }
}

fn options() -> egui_kittest::SnapshotOptions {
    egui_kittest::SnapshotOptions::new().threshold(1.0).max_failed_pixels(400)
}

/// The live fixture, its view settled.
fn settled(name: &str) -> Option<TestApp> {
    let mut t = TestApp::live(name);
    t.h.remove_cursor();
    t.frames(4);
    renders(&mut t).then_some(t)
}

/// Compares the last frame, with what changes from run to run masked: the CPU load and the
/// toast naming the folder the project was opened from.
fn snap(t: &mut TestApp, name: &str) {
    t.h.mask(egui::Rect::from_min_max(egui::pos2(1030.0, 24.0), egui::pos2(1200.0, 68.0)));
    t.h.mask(egui::Rect::from_min_max(egui::pos2(0.0, SIZE.y - 60.0), egui::pos2(SIZE.x, SIZE.y - 12.0)));
    t.h.snapshot_options(name, &options());
}

#[test]
fn arrange_view_snapshot() {
    let Some(mut t) = settled("snapshot_arrange") else { return };
    snap(&mut t, "arrange");
}

#[test]
fn setlist_page_snapshot() {
    let Some(mut t) = settled("snapshot_setlist") else { return };
    t.click("Setlist (L)");
    t.h.remove_cursor();
    t.frames(2);
    snap(&mut t, "setlist");
}
