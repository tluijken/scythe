//! The colour theme of Scythe's windows: a colour per main UI element, saved with the
//! preferences, and REAPER theme colours imported where they map.

use std::path::PathBuf;

use schemars::JsonSchema;
use scythe_core::theme::{Rgb, Slot, Theme};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};

use super::ApiError;
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum ThemeCommand {
    /// Every themable colour: its key, what it colours, its value and Scythe's default, and
    /// the REAPER theme key it is imported from.
    #[serde(rename = "theme.get")]
    Get,
    /// Sets colours, as `{"accent": "#fd971f", ...}` with keys from `theme.get`.
    #[serde(rename = "theme.set")]
    Set { colors: std::collections::BTreeMap<String, String> },
    /// Back to Scythe's own colours.
    #[serde(rename = "theme.reset")]
    Reset,
    /// Loads a `.ScytheTheme`, or the colours of a REAPER `.ReaperTheme` or `.ReaperThemeZip`
    /// where they map (REAPER's images and layouts are not used).
    #[serde(rename = "theme.load")]
    Load { path: PathBuf },
    /// Saves the theme as a `.ScytheTheme` file.
    #[serde(rename = "theme.save")]
    Save { path: PathBuf },
}

fn describe(theme: &Theme) -> Value {
    let colors: Vec<Value> = Slot::ALL
        .into_iter()
        .map(|s| {
            value!({
                "key": s.key(), "label": s.label(), "color": theme.get(s).hex(),
                "default": s.default_color().hex(), "reaper_key": s.reaper_key(),
            })
        })
        .collect();
    value!({ "colors": colors, "default": theme.is_default() })
}

pub fn execute(session: &mut Session, command: ThemeCommand) -> Result<Value, ApiError> {
    match command {
        ThemeCommand::Get => Ok(describe(&session.theme)),
        ThemeCommand::Set { colors } => {
            let mut theme = session.theme.clone();
            for (k, v) in &colors {
                let slot = Slot::from_key(k).ok_or_else(|| ApiError::not_found(format!("no theme colour \"{k}\"")))?;
                let c = Rgb::parse(v).ok_or_else(|| ApiError::invalid(format!("\"{v}\" is not a #rrggbb colour")))?;
                theme.set(slot, c);
            }
            session.set_theme(theme);
            Ok(describe(&session.theme))
        }
        ThemeCommand::Reset => {
            session.set_theme(Theme::default());
            Ok(describe(&session.theme))
        }
        ThemeCommand::Load { path } => {
            let slots = session.load_theme_file(&path).map_err(ApiError::failed)?;
            let mut answer = describe(&session.theme);
            answer["loaded"] = slots.iter().map(|s| s.key()).collect::<Vec<_>>().into();
            Ok(answer)
        }
        ThemeCommand::Save { path } => {
            session.save_theme_file(&path).map_err(ApiError::failed)?;
            Ok(value!({ "path": path }))
        }
    }
}
