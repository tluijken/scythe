//! Scythe's colour theme: a colour for each of the main UI elements, kept in a `.ScytheTheme`
//! file (`[scythe theme]` with `slot=#rrggbb` lines), and the import of the colour keys of a
//! REAPER `.ReaperTheme` where they mean the same thing. REAPER's images and WALTER layouts
//! are not read.
//!
//! A `.ReaperTheme` stores colours under `[color theme]` as Windows `COLORREF` integers:
//! red in the low byte, then green and blue (REAPER 7.73's `GetThemeColor` agrees, measured on
//! the default theme). Bits above the low 24 are flags and some values are negative for that
//! reason; only the colour is taken.

use crate::ini::Ini;

pub const SECTION: &str = "scythe theme";
pub const EXTENSION: &str = "ScytheTheme";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    pub fn parse(text: &str) -> Option<Self> {
        let h = text.trim().trim_start_matches('#');
        if h.len() != 6 || !h.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Self(byte(0)?, byte(2)?, byte(4)?))
    }

    /// A REAPER theme value (`COLORREF`, red in the low byte).
    pub fn from_reaper(value: i64) -> Self {
        let v = value as u32;
        Self(v as u8, (v >> 8) as u8, (v >> 16) as u8)
    }

    pub fn to_reaper(self) -> i64 {
        self.0 as i64 | (self.1 as i64) << 8 | (self.2 as i64) << 16
    }

    /// `self` moved `t` (0 to 1) of the way to `other`.
    pub fn mix(self, other: Self, t: f32) -> Self {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
        Self(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

/// The themable colours. Each is one colour of Scythe's palette, so changing it recolours
/// everything drawn with that colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Background,
    Panel,
    Raised,
    RaisedHighlight,
    Lane,
    LaneSelected,
    Text,
    TextMid,
    TextDim,
    SwitchOff,
    ArmBackground,
    ArmOff,
    Yellow,
    Accent,
    Green,
    Pink,
}

impl Slot {
    pub const ALL: [Slot; 16] = [
        Slot::Background,
        Slot::Panel,
        Slot::Raised,
        Slot::RaisedHighlight,
        Slot::Lane,
        Slot::LaneSelected,
        Slot::Text,
        Slot::TextMid,
        Slot::TextDim,
        Slot::SwitchOff,
        Slot::ArmBackground,
        Slot::ArmOff,
        Slot::Yellow,
        Slot::Accent,
        Slot::Green,
        Slot::Pink,
    ];

    /// The key in a `.ScytheTheme` file and the control API.
    pub fn key(self) -> &'static str {
        match self {
            Slot::Background => "background",
            Slot::Panel => "panel",
            Slot::Raised => "raised",
            Slot::RaisedHighlight => "raised_highlight",
            Slot::Lane => "lane",
            Slot::LaneSelected => "lane_selected",
            Slot::Text => "text",
            Slot::TextMid => "text_mid",
            Slot::TextDim => "text_dim",
            Slot::SwitchOff => "switch_off",
            Slot::ArmBackground => "arm_background",
            Slot::ArmOff => "arm_off",
            Slot::Yellow => "yellow",
            Slot::Accent => "accent",
            Slot::Green => "green",
            Slot::Pink => "pink",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Slot::Background => "Background (arrange, meters)",
            Slot::Panel => "Panels and windows",
            Slot::Raised => "Buttons and fields",
            Slot::RaisedHighlight => "Hovered buttons, borders",
            Slot::Lane => "Track lane",
            Slot::LaneSelected => "Selected track lane",
            Slot::Text => "Text",
            Slot::TextMid => "Secondary text",
            Slot::TextDim => "Dim text, grid",
            Slot::SwitchOff => "Switch off, slider rail",
            Slot::ArmBackground => "Armed button",
            Slot::ArmOff => "Arm off, hovered switch",
            Slot::Yellow => "Mute, trim and tempo (yellow)",
            Slot::Accent => "Accent: selection, loop, pan (orange)",
            Slot::Green => "Volume, meters, play (green)",
            Slot::Pink => "Record, arm, mute envelope (pink)",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key().eq_ignore_ascii_case(key.trim()))
    }

    /// Scythe's own colour.
    pub fn default_color(self) -> Rgb {
        match self {
            Slot::Background => Rgb(0x1a, 0x1b, 0x18),
            Slot::Panel => Rgb(0x25, 0x26, 0x20),
            Slot::Raised => Rgb(0x2e, 0x2f, 0x29),
            Slot::RaisedHighlight => Rgb(0x3a, 0x3b, 0x33),
            Slot::Lane => Rgb(0x22, 0x23, 0x1e),
            Slot::LaneSelected => Rgb(0x2b, 0x2c, 0x26),
            Slot::Text => Rgb(0xf8, 0xf8, 0xf2),
            Slot::TextMid => Rgb(0xd6, 0xd6, 0xcc),
            Slot::TextDim => Rgb(0xa6, 0xa2, 0x8c),
            Slot::SwitchOff => Rgb(0x49, 0x48, 0x3e),
            Slot::ArmBackground => Rgb(0x4a, 0x22, 0x30),
            Slot::ArmOff => Rgb(0x75, 0x71, 0x5e),
            Slot::Yellow => Rgb(0xe6, 0xdb, 0x74),
            Slot::Accent => Rgb(0xfd, 0x97, 0x1f),
            Slot::Green => Rgb(0xa6, 0xe2, 0x2e),
            Slot::Pink => Rgb(0xf9, 0x26, 0x72),
        }
    }

    /// The `.ReaperTheme` key whose colour means the same, for the slots REAPER has one for.
    /// The envelope colours carry over because Scythe draws its envelopes in its palette's
    /// green (volume), yellow (trim), orange (pan) and pink (mute).
    pub fn reaper_key(self) -> Option<&'static str> {
        Some(match self {
            Slot::Background => "col_arrangebg",
            Slot::Panel => "col_main_bg2",
            Slot::Lane => "col_tr1_bg",
            Slot::LaneSelected => "selcol_tr1_bg",
            Slot::Text => "col_main_text2",
            Slot::TextMid => "col_tl_fg",
            Slot::TextDim => "col_tl_fg2",
            Slot::Green => "col_env1",
            Slot::Yellow => "env_trim_vol",
            Slot::Accent => "col_env3",
            Slot::Pink => "env_track_mute",
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    colors: [Rgb; Slot::ALL.len()],
}

impl Default for Theme {
    fn default() -> Self {
        Self { colors: Slot::ALL.map(Slot::default_color) }
    }
}

impl Theme {
    pub fn get(&self, slot: Slot) -> Rgb {
        self.colors[slot as usize]
    }

    pub fn set(&mut self, slot: Slot, color: Rgb) {
        self.colors[slot as usize] = color;
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Reads a `.ScytheTheme`; missing or unreadable slots keep Scythe's colour.
    pub fn parse(text: &str) -> Self {
        let mut theme = Self::default();
        if let Some(s) = Ini::parse(text).section(SECTION) {
            for (k, v) in &s.entries {
                if let (Some(slot), Some(c)) = (Slot::from_key(k), Rgb::parse(v)) {
                    theme.set(slot, c);
                }
            }
        }
        theme
    }

    pub fn write(&self) -> String {
        let mut ini = Ini::default();
        let s = ini.section_mut(SECTION);
        for slot in Slot::ALL {
            s.set(slot.key(), &self.get(slot).hex());
        }
        ini.write()
    }

    /// The colours of a `.ReaperTheme` where they map, the in-between shades of Scythe's
    /// buttons derived from the imported panel and text colours. Answers the theme and the
    /// slots that came from REAPER keys.
    pub fn from_reaper(text: &str) -> (Self, Vec<Slot>) {
        let ini = Ini::parse(text);
        let mut theme = Self::default();
        let mut mapped = Vec::new();
        let Some(colors) = ini.section("color theme") else { return (theme, mapped) };
        for slot in Slot::ALL {
            let value = slot.reaper_key().and_then(|k| colors.get(k)).and_then(|v| v.trim().parse::<i64>().ok());
            if let Some(v) = value {
                theme.set(slot, Rgb::from_reaper(v));
                mapped.push(slot);
            }
        }
        let (panel, text) = (theme.get(Slot::Panel), theme.get(Slot::Text));
        if mapped.contains(&Slot::Panel) || mapped.contains(&Slot::Text) {
            theme.set(Slot::Raised, panel.mix(text, 0.07));
            theme.set(Slot::RaisedHighlight, panel.mix(text, 0.14));
            theme.set(Slot::SwitchOff, panel.mix(text, 0.22));
            theme.set(Slot::ArmOff, panel.mix(text, 0.38));
        }
        if mapped.contains(&Slot::Pink) || mapped.contains(&Slot::Panel) {
            theme.set(Slot::ArmBackground, panel.mix(theme.get(Slot::Pink), 0.3));
        }
        (theme, mapped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaper_colors_are_colorref() {
        // REAPER 7.73's default theme and what GetThemeColor answers for the same keys.
        assert_eq!(Rgb::from_reaper(8886323), Rgb(51, 152, 135));
        assert_eq!(Rgb::from_reaper(5425391), Rgb(239, 200, 82));
        assert_eq!(Rgb::from_reaper(160), Rgb(160, 0, 0));
        assert_eq!(Rgb::from_reaper(-2135838293), Rgb(171, 177, 177));
        assert_eq!(Rgb::from_reaper(-11316397), Rgb(83, 83, 83));
        assert_eq!(Rgb(51, 152, 135).to_reaper(), 8886323);
    }

    #[test]
    fn imports_the_mapped_reaper_keys() {
        let text = "[color theme]\ncol_main_bg2=3355443\ncol_main_text2=9211020\ncol_arrangebg=4539717\n\
                    col_tr1_bg=4342338\ncol_env1=65280\n[REAPER]\nui_img=x\n";
        let (theme, mapped) = Theme::from_reaper(text);
        assert_eq!(mapped, [Slot::Background, Slot::Panel, Slot::Lane, Slot::Text, Slot::Green]);
        assert_eq!(theme.get(Slot::Panel), Rgb(51, 51, 51));
        assert_eq!(theme.get(Slot::Background), Rgb(69, 69, 69));
        assert_eq!(theme.get(Slot::Green), Rgb(0, 255, 0));
        assert_eq!(theme.get(Slot::Raised), Rgb(51, 51, 51).mix(Rgb(140, 140, 140), 0.07));
        assert_eq!(theme.get(Slot::Accent), Slot::Accent.default_color());
        assert_eq!(Theme::from_reaper("[REAPER]\n").1, []);
    }

    #[test]
    fn theme_file_round_trips() {
        let mut t = Theme::default();
        assert!(t.is_default());
        t.set(Slot::Accent, Rgb(1, 2, 3));
        let text = t.write();
        assert!(text.starts_with("[scythe theme]\nbackground=#1a1b18\n"));
        assert!(text.contains("accent=#010203\n"));
        assert_eq!(Theme::parse(&text), t);
        assert_eq!(Theme::parse("[scythe theme]\ntext=nonsense\nbogus=#ffffff\n"), Theme::default());
    }
}
