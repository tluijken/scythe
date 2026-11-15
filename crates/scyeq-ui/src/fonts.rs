//! Inter and JetBrains Mono, the faces Scythe and ScyEQ-Pro draw with.

use std::sync::Arc;

use egui::FontFamily;
use egui::epaint::text::{FontData, FontDefinitions};

use crate::theme::SEMIBOLD;

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
}
