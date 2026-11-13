//! Trying TONE3000 models in a slot before keeping them. A preview fetches the model into a
//! cache outside the library and plays it in place of the slot's model, remembering what the
//! slot played before: Keep moves the model into the library, Revert puts the old one back.

use std::path::{Path, PathBuf};

use super::Target;
use super::tone3000::{Model, Tone};

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    Fetching,
    Loading,
    Playing,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub target: Target,
    /// The model the slot played before the first preview, as the slot names it.
    pub previous: Option<String>,
    pub tone: Tone,
    /// The tone's models, stepped through with the arrow keys.
    pub models: Vec<Model>,
    at: usize,
    pub stage: Stage,
    /// The slot plays `previous` for comparison while this is set.
    pub comparing: bool,
    /// The preview has changed the slot, in an undo step later changes fold into.
    pub recorded: bool,
    /// The tone holds impulse responses, which play in the rig's cab.
    pub ir: bool,
    /// Whether the rig's cab played before the preview, put back when comparing.
    pub cab_before: Option<bool>,
}

/// What ending a preview leaves to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Ending {
    /// Store the auditioned model in the library and point the slot at it.
    Keep { target: Target, tone: Box<Tone>, model: Box<Model> },
    /// Put the slot's earlier model back.
    Revert { target: Target, previous: Option<String> },
}

impl Preview {
    /// Starts previewing `model_id` of `tone`; `None` when the tone has no such model.
    pub fn new(
        target: Target,
        previous: Option<String>,
        tone: Tone,
        models: Vec<Model>,
        model_id: i64,
    ) -> Option<Self> {
        let at = models.iter().position(|m| m.id == model_id)?;
        let ir = tone.is_ir();
        Some(Self {
            target,
            previous,
            tone,
            models,
            at,
            stage: Stage::Fetching,
            comparing: false,
            recorded: false,
            ir,
            cab_before: None,
        })
    }

    /// Moves on to another model, of this tone or another, keeping what the slot played
    /// before the preview began.
    pub fn switch(&mut self, tone: Tone, models: Vec<Model>, model_id: i64) -> bool {
        let Some(at) = models.iter().position(|m| m.id == model_id) else { return false };
        self.ir = tone.is_ir();
        self.tone = tone;
        self.models = models;
        self.at = at;
        self.stage = Stage::Fetching;
        self.comparing = false;
        true
    }

    pub fn model(&self) -> &Model {
        &self.models[self.at]
    }

    pub fn position(&self) -> (usize, usize) {
        (self.at + 1, self.models.len())
    }

    /// The model `by` places away, wrapping around the tone's models.
    pub fn step(&mut self, by: isize) -> &Model {
        let n = self.models.len() as isize;
        self.at = (self.at as isize + by).rem_euclid(n) as usize;
        self.stage = Stage::Fetching;
        self.comparing = false;
        self.model()
    }

    /// Whether a model read for `target` is the one the preview still wants playing.
    pub fn wants(&self, target: &Target, model_id: i64) -> bool {
        self.target == *target && self.model().id == model_id && !self.comparing
    }

    /// Switches between the auditioned model and the earlier one; true when the earlier one
    /// plays now.
    pub fn toggle_compare(&mut self) -> bool {
        self.comparing = !self.comparing;
        self.comparing
    }

    pub fn keep(self) -> Ending {
        let model = Box::new(self.model().clone());
        Ending::Keep { target: self.target, tone: Box::new(self.tone), model }
    }

    pub fn revert(self) -> Ending {
        Ending::Revert { target: self.target, previous: self.previous }
    }
}

/// The model a tone's preview starts with: the first of `size` when asked for, else the first
/// full-size one, else the first.
pub fn first_model<'a>(models: &'a [Model], size: Option<&str>) -> Option<&'a Model> {
    let of = |s: &str| models.iter().find(|m| m.size.as_deref().is_some_and(|z| z.eq_ignore_ascii_case(s)));
    size.and_then(of).or_else(|| of("standard")).or_else(|| models.first())
}

/// Where a previewed model is cached until it is kept or the cache is trimmed.
pub fn cache_file(dir: &Path, model: &Model, ir: bool) -> PathBuf {
    let name: String = model
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '(' | ')') { c } else { '_' })
        .take(60)
        .collect();
    let name = name.trim();
    let ext = if ir { "wav" } else { "nam" };
    dir.join(if name.is_empty() { format!("{}.{ext}", model.id) } else { format!("{} {name}.{ext}", model.id) })
}

/// `$XDG_CACHE_HOME/scythe/nam-preview`.
pub fn default_cache_dir() -> PathBuf {
    scythe_core::paths::cache_home().unwrap_or_else(std::env::temp_dir).join("scythe").join("nam-preview")
}

/// Keeps the newest `keep` files of the cache.
pub fn trim_cache(dir: &Path, keep: usize) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = read
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(_, p)| p.is_file())
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: i64, size: &str) -> Model {
        Model { id, tone_id: 7, name: format!("m{id}"), size: Some(size.into()), ..Model::default() }
    }

    fn target() -> Target {
        Target { track: scythe_core::TrackId(1), fx_id: "{FX}".into() }
    }

    fn tone(id: i64) -> Tone {
        Tone { id, title: format!("tone {id}"), ..Tone::default() }
    }

    #[test]
    fn steps_through_a_tones_models_and_keeps_what_played_before() {
        let models = vec![model(1, "standard"), model(2, "lite"), model(3, "nano")];
        let mut p = Preview::new(target(), Some("/lib/old.nam".into()), tone(7), models.clone(), 2).unwrap();
        assert_eq!((p.model().id, p.position(), &p.stage), (2, (2, 3), &Stage::Fetching));
        assert!(Preview::new(target(), None, tone(7), models.clone(), 99).is_none());
        assert_eq!(p.step(1).id, 3);
        assert_eq!(p.step(1).id, 1, "wraps around");
        assert_eq!(p.step(-1).id, 3);
        assert!(p.wants(&target(), 3));
        assert!(!p.wants(&target(), 1), "a model stepped past no longer loads");
        let other = Target { fx_id: "{OTHER}".into(), ..target() };
        assert!(!p.wants(&other, 3));

        assert!(p.switch(tone(8), vec![model(10, "standard")], 10));
        assert!(!p.switch(tone(9), vec![model(11, "standard")], 12));
        assert_eq!((p.tone.id, p.model().id), (8, 10));
        assert_eq!(p.previous.as_deref(), Some("/lib/old.nam"), "the first model is what Revert restores");
    }

    #[test]
    fn compare_keep_and_revert() {
        let mut p = Preview::new(target(), None, tone(7), vec![model(1, "standard"), model(2, "lite")], 1).unwrap();
        p.stage = Stage::Playing;
        assert!(p.toggle_compare());
        assert!(!p.wants(&target(), 1), "the earlier model plays while comparing");
        assert!(!p.toggle_compare());
        assert!(p.wants(&target(), 1));
        p.toggle_compare();
        p.step(1);
        assert!(!p.comparing, "stepping plays the next model");
        match p.clone().keep() {
            Ending::Keep { target: t, tone, model } => assert_eq!((t, tone.id, model.id), (target(), 7, 2)),
            other => panic!("{other:?}"),
        }
        assert_eq!(p.revert(), Ending::Revert { target: target(), previous: None });
    }

    #[test]
    fn picks_the_first_model_and_caches_outside_the_library() {
        let models = vec![model(1, "nano"), model(2, "standard"), model(3, "lite")];
        assert_eq!(first_model(&models, None).unwrap().id, 2);
        assert_eq!(first_model(&models, Some("lite")).unwrap().id, 3);
        assert_eq!(first_model(&models, Some("feather")).unwrap().id, 2);
        assert_eq!(first_model(&models[..1], None).unwrap().id, 1);
        assert!(first_model(&[], None).is_none());
        assert_eq!(cache_file(Path::new("/c"), &models[0], false), PathBuf::from("/c/1 m1.nam"));
        let odd = Model { id: 5, name: "a/b: c".into(), ..Model::default() };
        assert_eq!(cache_file(Path::new("/c"), &odd, false), PathBuf::from("/c/5 a_b_ c.nam"));
        assert_eq!(cache_file(Path::new("/c"), &Model { id: 6, ..Model::default() }, true), PathBuf::from("/c/6.wav"));

        let dir = std::env::temp_dir().join(format!("scythe-preview-trim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..5 {
            let f = dir.join(format!("{n}.nam"));
            std::fs::write(&f, "x").unwrap();
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + n);
            std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
        }
        trim_cache(&dir, 2);
        let mut left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        left.sort();
        assert_eq!(left, ["3.nam", "4.nam"]);
        std::fs::remove_dir_all(dir).ok();
    }
}
