//! The colour theme: kept in `theme.ScytheTheme` beside the preferences, edited colour by
//! colour, saved and loaded as files, and imported from REAPER's `.ReaperTheme` and
//! `.ReaperThemeZip` (colours only).

use std::io::Read;
use std::path::{Path, PathBuf};

use scythe_core::theme::{Rgb, Slot, Theme};

use crate::Session;

pub const THEME_FILE: &str = "theme.ScytheTheme";

impl Session {
    fn theme_file(&self) -> Option<PathBuf> {
        Some(self.prefs_file.as_ref()?.with_file_name(THEME_FILE))
    }

    pub(crate) fn load_theme(&mut self) {
        self.theme = self
            .theme_file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map_or_else(Theme::default, |t| Theme::parse(&t));
    }

    /// Makes `theme` the current one and keeps it for the next start.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        if let Some(path) = self.theme_file()
            && let Err(e) = std::fs::write(&path, self.theme.write())
        {
            self.notify(format!("Cannot save {}: {e}", path.display()));
        }
        self.sync();
    }

    pub fn set_theme_color(&mut self, slot: Slot, color: Rgb) {
        let mut t = self.theme.clone();
        t.set(slot, color);
        self.set_theme(t);
    }

    /// Loads a `.ScytheTheme`, or the colours of a `.ReaperTheme`/`.ReaperThemeZip`. Answers
    /// the slots taken from the file.
    pub fn load_theme_file(&mut self, path: &Path) -> Result<Vec<Slot>, String> {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let (theme, slots) = match ext.as_str() {
            "reapertheme" => {
                Theme::from_reaper(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
            }
            "reaperthemezip" => {
                let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                Theme::from_reaper(&theme_in_zip(&bytes)?)
            }
            _ => {
                let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
                if !text.lines().any(|l| l.trim().eq_ignore_ascii_case("[scythe theme]")) {
                    return Err(format!("{} is not a Scythe theme", path.display()));
                }
                (Theme::parse(&text), Slot::ALL.to_vec())
            }
        };
        self.set_theme(theme);
        Ok(slots)
    }

    pub fn save_theme_file(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, self.theme.write()).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The `.ReaperTheme` inside a `.ReaperThemeZip` (a zip archive with the theme's images).
fn theme_in_zip(zip: &[u8]) -> Result<String, String> {
    let u16_at = |at: usize| zip.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |at: usize| zip.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let end = (0..zip.len().saturating_sub(21))
        .rev()
        .find(|&i| zip[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or("not a zip archive")?;
    let count = u16_at(end + 10).ok_or("bad zip")?;
    let mut at = u32_at(end + 16).ok_or("bad zip")?;
    for _ in 0..count {
        if !zip.get(at..).is_some_and(|z| z.starts_with(&[0x50, 0x4b, 0x01, 0x02])) {
            return Err("bad zip directory".into());
        }
        let method = u16_at(at + 10).ok_or("bad zip")?;
        let size = u32_at(at + 20).ok_or("bad zip")?;
        let (name_len, extra_len, comment_len) =
            (u16_at(at + 28).ok_or("bad zip")?, u16_at(at + 30).ok_or("bad zip")?, u16_at(at + 32).ok_or("bad zip")?);
        let local = u32_at(at + 42).ok_or("bad zip")?;
        let name = String::from_utf8_lossy(zip.get(at + 46..at + 46 + name_len).ok_or("bad zip")?).into_owned();
        at += 46 + name_len + extra_len + comment_len;
        if !name.to_ascii_lowercase().ends_with(".reapertheme") {
            continue;
        }
        let data_at = local + 30 + u16_at(local + 26).ok_or("bad zip")? + u16_at(local + 28).ok_or("bad zip")?;
        let data = zip.get(data_at..data_at + size).ok_or("bad zip")?;
        return match method {
            0 => Ok(String::from_utf8_lossy(data).into_owned()),
            8 => {
                let mut bytes = Vec::new();
                flate2::read::DeflateDecoder::new(data).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            m => Err(format!("unsupported zip compression {m}")),
        };
    }
    Err("no .ReaperTheme in the archive".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored (uncompressed) zip with one file.
    fn zip_of(name: &str, data: &[u8]) -> Vec<u8> {
        let mut z = Vec::new();
        let header = |z: &mut Vec<u8>, sig: [u8; 4]| z.extend(sig);
        header(&mut z, [0x50, 0x4b, 0x03, 0x04]);
        z.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend((data.len() as u32).to_le_bytes());
        z.extend((data.len() as u32).to_le_bytes());
        z.extend((name.len() as u16).to_le_bytes());
        z.extend(0u16.to_le_bytes());
        z.extend(name.as_bytes());
        z.extend(data);
        let cd = z.len();
        header(&mut z, [0x50, 0x4b, 0x01, 0x02]);
        z.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend((data.len() as u32).to_le_bytes());
        z.extend((data.len() as u32).to_le_bytes());
        z.extend((name.len() as u16).to_le_bytes());
        z.extend([0u8; 12]);
        z.extend(0u32.to_le_bytes());
        z.extend(name.as_bytes());
        let cd_len = z.len() - cd;
        header(&mut z, [0x50, 0x4b, 0x05, 0x06]);
        z.extend([0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend((cd_len as u32).to_le_bytes());
        z.extend((cd as u32).to_le_bytes());
        z.extend([0, 0]);
        z
    }

    #[test]
    fn reads_the_theme_out_of_a_zip() {
        let z = zip_of("My.ReaperTheme", b"[color theme]\ncol_main_bg2=255\n");
        assert_eq!(theme_in_zip(&z).unwrap(), "[color theme]\ncol_main_bg2=255\n");
        assert!(theme_in_zip(&zip_of("a.png", b"x")).is_err());
        assert!(theme_in_zip(b"nope").is_err());
    }

    /// REAPER 7.73's own default theme, when the REAPER install is around.
    #[test]
    fn imports_reapers_default_theme() {
        let Ok(dir) = std::env::var("SCYTHE_REAPER_THEMES") else { return };
        let bytes = std::fs::read(Path::new(&dir).join("Default_7.0.ReaperThemeZip")).unwrap();
        let (theme, mapped) = Theme::from_reaper(&theme_in_zip(&bytes).unwrap());
        assert_eq!(mapped.len(), 11);
        assert_eq!(theme.get(Slot::Panel), Rgb(51, 51, 51));
    }
}
