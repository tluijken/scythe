use std::sync::Arc;

use egui::epaint::text::{FontData, FontDefinitions};
use egui::{Color32, FontFamily, FontId};
use scythe_core::Color;

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

pub fn track_color(c: Option<Color>) -> Color32 {
    c.map_or(TEXT_DIM, |c| Color32::from_rgb(c.r, c.g, c.b))
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

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add("inter", include_bytes!("../assets/fonts/Inter-Regular.ttf"));
    add("inter-semibold", include_bytes!("../assets/fonts/Inter-SemiBold.ttf"));
    add("jetbrains-mono", include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"));

    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let family =
        |primary: &str| std::iter::once(primary.to_owned()).chain(fallbacks.iter().cloned()).collect::<Vec<_>>();
    fonts.families.insert(FontFamily::Proportional, family("inter"));
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), family("inter-semibold"));
    fonts.families.insert(FontFamily::Monospace, family("jetbrains-mono"));
    ctx.set_fonts(fonts);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = with_alpha(ACCENT, 90);
    visuals.selection.stroke.color = ACCENT;
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.inactive.bg_fill = RAISED;
    visuals.widgets.inactive.weak_bg_fill = RAISED;
    visuals.widgets.hovered.bg_fill = RAISED_HI;
    visuals.widgets.hovered.weak_bg_fill = RAISED_HI;
    visuals.widgets.active.bg_fill = RAISED_HI;
    visuals.window_stroke.color = RAISED_HI;
    ctx.set_visuals(visuals);
}
