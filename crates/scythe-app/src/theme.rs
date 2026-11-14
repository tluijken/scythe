use std::sync::Arc;

use egui::Color32;
pub use scyeq_ui::theme::*;
use scythe_core::Color;
use scythe_core::theme::{Slot, Theme};

/// A track's colour. One that equals a palette colour is moved off it by a few steps of blue (more than [`recolor`] allows for rounding), so a
/// theme ([`recolor`]) leaves the user's track colours alone.
pub fn track_color(c: Option<Color>) -> Color32 {
    c.map_or(TEXT_DIM, |c| {
        let rgb = Color32::from_rgb(c.r, c.g, c.b);
        if Slot::ALL.into_iter().any(|s| palette(s) == rgb) { Color32::from_rgb(c.r, c.g, c.b ^ 8) } else { rgb }
    })
}

/// The palette colour each themable slot stands for.
fn palette(slot: Slot) -> Color32 {
    let c = slot.default_color();
    Color32::from_rgb(c.0, c.1, c.2)
}

/// Recolours what was painted this frame from Scythe's palette to `theme`.
///
/// The palette is constants used throughout the UI, so a theme is applied to the shapes the
/// frame painted rather than to every place that names a colour: each palette colour (also
/// with an alpha, as `with_alpha` makes them) becomes the theme's colour for its slot.
pub fn recolor(ctx: &egui::Context, theme: &Theme) {
    if theme.is_default() {
        return;
    }
    let map: Vec<([u8; 3], [u8; 3])> = Slot::ALL
        .into_iter()
        .filter(|s| theme.get(*s) != s.default_color())
        .map(|s| {
            let (from, to) = (palette(s), theme.get(s));
            ([from.r(), from.g(), from.b()], [to.0, to.1, to.2])
        })
        .collect();
    let layers: Vec<egui::LayerId> = ctx.memory(|m| m.layer_ids().collect());
    ctx.graphics_mut(|g| {
        for layer in layers {
            let Some(list) = g.get_mut(layer) else { continue };
            for i in 0..list.next_idx().0 {
                list.mutate_shape(egui::layers::ShapeIdx(i), |c| recolor_shape(&mut c.shape, &map));
            }
        }
    });
}

fn swap(c: &mut Color32, map: &[([u8; 3], [u8; 3])]) {
    if c.a() == 0 {
        return;
    }
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    // Premultiplying and back loses precision at low alpha.
    let tolerance = if a == 255 { 0 } else { (255 / a as i32).clamp(1, 6) };
    let near = |x: u8, y: u8| (x as i32 - y as i32).abs() <= tolerance;
    if let Some((_, to)) = map.iter().find(|(from, _)| near(from[0], r) && near(from[1], g) && near(from[2], b)) {
        *c = Color32::from_rgba_unmultiplied(to[0], to[1], to[2], a);
    }
}

fn swap_mode(c: &mut egui::epaint::ColorMode, map: &[([u8; 3], [u8; 3])]) {
    if let egui::epaint::ColorMode::Solid(c) = c {
        swap(c, map);
    }
}

fn recolor_shape(shape: &mut egui::Shape, map: &[([u8; 3], [u8; 3])]) {
    use egui::Shape;
    match shape {
        Shape::Vec(v) => v.iter_mut().for_each(|s| recolor_shape(s, map)),
        Shape::Circle(c) => {
            swap(&mut c.fill, map);
            swap(&mut c.stroke.color, map);
        }
        Shape::Ellipse(e) => {
            swap(&mut e.fill, map);
            swap(&mut e.stroke.color, map);
        }
        Shape::LineSegment { stroke, .. } => swap(&mut stroke.color, map),
        Shape::Path(p) => {
            swap(&mut p.fill, map);
            swap_mode(&mut p.stroke.color, map);
        }
        Shape::Rect(r) => {
            swap(&mut r.fill, map);
            swap(&mut r.stroke.color, map);
        }
        Shape::Text(t) => {
            swap(&mut t.fallback_color, map);
            if let Some(c) = &mut t.override_text_color {
                swap(c, map);
            }
            swap(&mut t.underline.color, map);
            let galley = Arc::make_mut(&mut t.galley);
            for placed in &mut galley.rows {
                let row = Arc::make_mut(&mut placed.row);
                for v in &mut row.visuals.mesh.vertices {
                    swap(&mut v.color, map);
                }
            }
        }
        Shape::Mesh(m) => {
            for v in &mut Arc::make_mut(m).vertices {
                swap(&mut v.color, map);
            }
        }
        Shape::QuadraticBezier(b) => {
            swap(&mut b.fill, map);
            swap_mode(&mut b.stroke.color, map);
        }
        Shape::CubicBezier(b) => {
            swap(&mut b.fill, map);
            swap_mode(&mut b.stroke.color, map);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_slots_match_the_constants() {
        let consts = [
            (Slot::Background, BG),
            (Slot::Panel, PANEL),
            (Slot::Raised, RAISED),
            (Slot::RaisedHighlight, RAISED_HI),
            (Slot::Lane, LANE),
            (Slot::LaneSelected, LANE_SELECTED),
            (Slot::Text, TEXT),
            (Slot::TextMid, TEXT_MID),
            (Slot::TextDim, TEXT_DIM),
            (Slot::SwitchOff, SWITCH_OFF),
            (Slot::ArmBackground, ARM_BG),
            (Slot::ArmOff, ARM_OFF),
            (Slot::Yellow, YELLOW),
            (Slot::Accent, ACCENT),
            (Slot::Green, GREEN),
            (Slot::Pink, PINK),
        ];
        for (slot, c) in consts {
            assert_eq!(palette(slot), c, "{slot:?}");
        }
    }

    #[test]
    fn track_colours_stay_off_the_palette() {
        let c = track_color(Some(Color { r: 0xfd, g: 0x97, b: 0x1f }));
        assert_ne!(c, ACCENT);
        let mut faint = with_alpha(c, 0x30);
        swap(&mut faint, &[([0xfd, 0x97, 0x1f], [0, 0, 255])]);
        assert_eq!(faint, with_alpha(c, 0x30));
    }

    #[test]
    fn swaps_opaque_and_translucent_palette_colours() {
        let map = [([0xfd, 0x97, 0x1f], [0, 0, 255])];
        let mut c = ACCENT;
        swap(&mut c, &map);
        assert_eq!(c, Color32::from_rgb(0, 0, 255));
        let mut faint = with_alpha(ACCENT, 0x22);
        swap(&mut faint, &map);
        assert_eq!(faint, Color32::from_rgba_unmultiplied(0, 0, 255, 0x22));
        let mut other = Color32::from_rgb(1, 2, 3);
        swap(&mut other, &map);
        assert_eq!(other, Color32::from_rgb(1, 2, 3));
    }
}
