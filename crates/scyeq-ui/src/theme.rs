//! Scythe's palette and type, which ScyEQ-Pro's editor shares wherever it runs.

use egui::{Color32, FontFamily, FontId};

pub const BG: Color32 = Color32::from_rgb(0x1a, 0x1b, 0x18);
pub const PANEL: Color32 = Color32::from_rgb(0x25, 0x26, 0x20);
pub const RAISED: Color32 = Color32::from_rgb(0x2e, 0x2f, 0x29);
pub const RAISED_HI: Color32 = Color32::from_rgb(0x3a, 0x3b, 0x33);
pub const LANE: Color32 = Color32::from_rgb(0x22, 0x23, 0x1e);
pub const LANE_SELECTED: Color32 = Color32::from_rgb(0x2b, 0x2c, 0x26);
pub const TEXT: Color32 = Color32::from_rgb(0xf8, 0xf8, 0xf2);
pub const TEXT_MID: Color32 = Color32::from_rgb(0xd6, 0xd6, 0xcc);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xa6, 0xa2, 0x8c);
pub const SWITCH_OFF: Color32 = Color32::from_rgb(0x49, 0x48, 0x3e);
pub const ARM_BG: Color32 = Color32::from_rgb(0x4a, 0x22, 0x30);
pub const ARM_OFF: Color32 = Color32::from_rgb(0x75, 0x71, 0x5e);

pub const YELLOW: Color32 = Color32::from_rgb(0xe6, 0xdb, 0x74);
pub const ORANGE: Color32 = Color32::from_rgb(0xfd, 0x97, 0x1f);
pub const GREEN: Color32 = Color32::from_rgb(0xa6, 0xe2, 0x2e);
pub const PINK: Color32 = Color32::from_rgb(0xf9, 0x26, 0x72);

pub const ACCENT: Color32 = ORANGE;

pub const SEMIBOLD: &str = "semibold";

/// Colours that tell bands apart.
pub const BAND_COLORS: [Color32; 8] = [
    Color32::from_rgb(0xfd, 0x97, 0x1f),
    Color32::from_rgb(0x66, 0xd9, 0xef),
    Color32::from_rgb(0xa6, 0xe2, 0x2e),
    Color32::from_rgb(0xae, 0x81, 0xff),
    Color32::from_rgb(0xf9, 0x26, 0x72),
    Color32::from_rgb(0xe6, 0xdb, 0x74),
    Color32::from_rgb(0x4e, 0xc9, 0xb0),
    Color32::from_rgb(0xff, 0x8c, 0xc6),
];

pub fn band_color(k: usize) -> Color32 {
    BAND_COLORS[k % BAND_COLORS.len()]
}

pub fn with_alpha(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// The fonts and egui's own widgets in the palette.
pub fn install(ctx: &egui::Context) {
    crate::fonts::install(ctx);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = with_alpha(ACCENT, 90);
    visuals.selection.stroke.color = ACCENT;
    visuals.widgets.noninteractive.bg_fill = PANEL;
    // `bg_fill` is what checkboxes, radio buttons and slider rails draw with; RAISED vanishes on a panel.
    visuals.widgets.inactive.bg_fill = SWITCH_OFF;
    visuals.widgets.inactive.weak_bg_fill = RAISED;
    visuals.widgets.hovered.bg_fill = ARM_OFF;
    visuals.widgets.hovered.weak_bg_fill = RAISED_HI;
    visuals.widgets.active.bg_fill = ARM_OFF;
    visuals.window_stroke.color = RAISED_HI;
    ctx.set_visuals(visuals);
}
