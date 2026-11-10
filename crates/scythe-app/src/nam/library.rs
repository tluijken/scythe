//! The model library: models and impulse responses downloaded from TONE3000 into Scythe's
//! data folder, and folders the user adds, which are only read. What is known about each file
//! (name, who modelled it, the gear, architecture and rate, where it came from) is kept in
//! `library.json` with the user's favourites, tags and recently used models.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use super::tone3000::{Model, Tone};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Model,
    Ir,
}

/// Where a downloaded file came from on TONE3000.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub tone_id: i64,
    pub model_id: i64,
    #[serde(default)]
    pub tone_title: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub gear: Option<String>,
    #[serde(default)]
    pub makes: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: Kind,
    pub name: String,
    #[serde(default)]
    pub author: Option<String>,
    /// Make and model of the captured gear.
    #[serde(default)]
    pub gear: Option<String>,
    /// amp, pedal, amp-cab …
    #[serde(default)]
    pub gear_type: Option<String>,
    #[serde(default)]
    pub tone_type: Option<String>,
    /// `WaveNet standard`, `LSTM` …
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub sample_rate: Option<f64>,
    #[serde(default)]
    pub origin: Option<Origin>,
    /// The added folder the file was found in; `None` for downloads.
    #[serde(default)]
    pub folder: Option<PathBuf>,
    #[serde(default)]
    pub favourite: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Modification time and length when the file was read, to skip unchanged files.
    #[serde(default)]
    pub stamp: (u64, u64),
    /// Why the file could not be read.
    #[serde(default)]
    pub error: Option<String>,
}

impl Entry {
    /// The folders between the added folder and the file (`Full stacks/Mesa`), which often
    /// say what a model is.
    pub fn location(&self) -> String {
        let parent = self.path.parent().unwrap_or(Path::new(""));
        match &self.folder {
            Some(root) => parent.strip_prefix(root).unwrap_or(parent).to_string_lossy().into_owned(),
            None => self.origin.as_ref().map_or_else(String::new, |o| o.tone_title.clone()),
        }
    }

    /// One line about what the model is: gear, type and who made it.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(g) = &self.gear {
            parts.push(g.clone());
        }
        if let Some(t) = self.gear_type.as_ref().filter(|t| self.gear.as_ref().is_none_or(|g| !g.contains(*t))) {
            parts.push(t.clone());
        }
        if let Some(t) = &self.tone_type {
            parts.push(t.clone());
        }
        if let Some(a) = &self.author {
            parts.push(format!("by {a}"));
        }
        parts.join(" · ")
    }

    fn haystack(&self) -> String {
        let mut text = vec![self.name.clone(), self.location()];
        text.extend(
            [&self.author, &self.gear, &self.gear_type, &self.tone_type, &self.architecture]
                .into_iter()
                .flatten()
                .cloned(),
        );
        text.extend(self.tags.iter().cloned());
        if let Some(o) = &self.origin {
            text.push(o.tone_title.clone());
            text.extend(o.makes.iter().cloned());
        }
        if let Some(f) = self.path.file_name() {
            text.push(f.to_string_lossy().into_owned());
        }
        text.join(" ").to_lowercase()
    }
}

/// Where library files come from.
#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    /// Downloaded from TONE3000.
    Downloads,
    /// An added folder.
    Folder(PathBuf),
}

/// What the browser shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    /// Words that must all occur in the name, gear, author, folder or tags.
    pub text: String,
    pub kind: Option<Kind>,
    pub favourites: bool,
    pub tag: Option<String>,
    pub place: Option<Place>,
}

impl Filter {
    pub fn matches(&self, entry: &Entry) -> bool {
        if self.kind.is_some_and(|k| k != entry.kind) || (self.favourites && !entry.favourite) {
            return false;
        }
        if self.tag.as_ref().is_some_and(|t| !entry.tags.iter().any(|e| e.eq_ignore_ascii_case(t))) {
            return false;
        }
        let place = match &self.place {
            None => true,
            Some(Place::Downloads) => entry.folder.is_none(),
            Some(Place::Folder(f)) => entry.folder.as_ref() == Some(f),
        };
        if !place {
            return false;
        }
        let hay = entry.haystack();
        self.text.split_whitespace().all(|w| hay.contains(&w.to_lowercase()))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    folders: Vec<PathBuf>,
    #[serde(default)]
    entries: Vec<Entry>,
    #[serde(default)]
    recent: Vec<PathBuf>,
}

/// What a download keeps beside its file, so the library can be rebuilt from the folder.
const ORIGIN_SUFFIX: &str = "t3k.json";
const RECENT: usize = 30;

pub struct Library {
    dir: PathBuf,
    pub folders: Vec<PathBuf>,
    entries: Vec<Entry>,
    recent: Vec<PathBuf>,
}

impl Library {
    /// `$XDG_DATA_HOME/scythe/nam`.
    pub fn default_dir() -> PathBuf {
        crate::demo::data_dir().join("nam")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn open(dir: PathBuf) -> Self {
        let saved: Saved = std::fs::read(dir.join("library.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { dir, folders: saved.folders, entries: saved.entries, recent: saved.recent }
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let saved = Saved { folders: self.folders.clone(), entries: self.entries.clone(), recent: self.recent.clone() };
        let tmp = self.dir.join("library.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&saved).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, self.dir.join("library.json"))
    }

    pub fn downloads_dir(&self) -> PathBuf {
        self.dir.join("tone3000")
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry(&self, path: &Path) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    fn entry_mut(&mut self, path: &Path) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.path == path)
    }

    /// The folders to read: the downloads (`None`) and each added folder.
    pub fn roots(&self) -> Vec<(PathBuf, Option<PathBuf>)> {
        std::iter::once((self.downloads_dir(), None))
            .chain(self.folders.iter().map(|f| (f.clone(), Some(f.clone()))))
            .collect()
    }

    /// Reads every model and impulse response under `roots`, reusing what `known` says about
    /// files that did not change. Slow for big folders: run it off the UI thread.
    pub fn scan(roots: &[(PathBuf, Option<PathBuf>)], known: &[Entry]) -> Vec<Entry> {
        let mut out = Vec::new();
        for (dir, folder) in roots {
            let mut files = Vec::new();
            walk(dir, &mut files);
            files.sort();
            for path in files {
                let Some(kind) = kind_of(&path) else { continue };
                let stamp = stamp(&path);
                match known.iter().find(|e| e.path == path && e.stamp == stamp && e.folder == *folder) {
                    Some(e) => out.push(e.clone()),
                    None => out.push(read_entry(&path, kind, folder.clone(), stamp)),
                }
            }
        }
        out
    }

    /// Takes a scan's result, keeping favourites and tags of files that are still there.
    pub fn merge(&mut self, scanned: Vec<Entry>) {
        let old = std::mem::take(&mut self.entries);
        self.entries = scanned
            .into_iter()
            .map(|mut e| {
                if let Some(o) = old.iter().find(|o| o.path == e.path) {
                    e.favourite = o.favourite;
                    e.tags = o.tags.clone();
                }
                e
            })
            .collect();
        self.entries
            .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.path.cmp(&b.path)));
    }

    pub fn add_folder(&mut self, folder: PathBuf) -> bool {
        if self.folders.contains(&folder) {
            return false;
        }
        self.folders.push(folder);
        true
    }

    pub fn remove_folder(&mut self, folder: &Path) {
        self.folders.retain(|f| f != folder);
        self.entries.retain(|e| e.folder.as_deref() != Some(folder));
    }

    pub fn search(&self, filter: &Filter) -> Vec<&Entry> {
        self.entries.iter().filter(|e| filter.matches(e)).collect()
    }

    pub fn set_favourite(&mut self, path: &Path, favourite: bool) {
        if let Some(e) = self.entry_mut(path) {
            e.favourite = favourite;
        }
    }

    pub fn set_tags(&mut self, path: &Path, tags: Vec<String>) {
        if let Some(e) = self.entry_mut(path) {
            let mut tags: Vec<String> =
                tags.into_iter().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()).collect();
            tags.dedup();
            e.tags = tags;
        }
    }

    /// Every tag in use, sorted.
    pub fn tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self.entries.iter().flat_map(|e| e.tags.iter().cloned()).collect();
        tags.sort_by_key(|t| t.to_lowercase());
        tags.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        tags
    }

    /// Notes that `path` was just loaded.
    pub fn touch(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_owned());
        self.recent.truncate(RECENT);
    }

    pub fn recent(&self) -> &[PathBuf] {
        &self.recent
    }

    /// The model `step` places from `current` among those `filter` shows, wrapping around.
    pub fn neighbour(&self, current: Option<&Path>, step: isize, filter: &Filter) -> Option<&Entry> {
        let list = self.search(filter);
        if list.is_empty() {
            return None;
        }
        let n = list.len() as isize;
        let at = current.and_then(|c| list.iter().position(|e| e.path == c));
        let next = match at {
            Some(i) => (i as isize + step).rem_euclid(n),
            None if step < 0 => n - 1,
            None => 0,
        };
        list.get(next as usize).copied()
    }

    /// Saves a model downloaded from TONE3000 under the downloads folder, with what TONE3000
    /// says about it beside it, and adds it to the library.
    pub fn store_download(&mut self, tone: &Tone, model: &Model, bytes: &[u8]) -> std::io::Result<Entry> {
        let is_wav = bytes.starts_with(b"RIFF");
        let dir = self.downloads_dir().join(format!("{}-{}", tone.id, file_safe(&tone.title)));
        std::fs::create_dir_all(&dir)?;
        let stem = if model.name.trim().is_empty() { format!("model-{}", model.id) } else { file_safe(&model.name) };
        let path = dir.join(format!("{stem}.{}", if is_wav { "wav" } else { "nam" }));
        std::fs::write(&path, bytes)?;
        let origin = Origin {
            tone_id: tone.id,
            model_id: model.id,
            tone_title: tone.title.clone(),
            author: tone.author().map(str::to_owned),
            gear: tone.gear.clone(),
            makes: tone.makes.iter().map(|m| m.name.clone()).collect(),
            url: tone.url.clone(),
            license: tone.license.clone(),
        };
        std::fs::write(origin_path(&path), serde_json::to_vec_pretty(&origin).map_err(std::io::Error::other)?)?;
        let entry = read_entry(&path, if is_wav { Kind::Ir } else { Kind::Model }, None, stamp(&path));
        let mut scanned: Vec<Entry> = self.entries.iter().filter(|e| e.path != path).cloned().collect();
        scanned.push(entry.clone());
        self.merge(scanned);
        Ok(entry)
    }

    /// Whether a model of `tone` is already downloaded.
    pub fn downloaded(&self, model_id: i64) -> Option<&Entry> {
        self.entries.iter().find(|e| e.origin.as_ref().is_some_and(|o| o.model_id == model_id))
    }
}

fn origin_path(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(ORIGIN_SUFFIX);
    file.with_file_name(name)
}

/// A name that is safe as a file name everywhere.
fn file_safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_owned();
    if trimmed.is_empty() { "untitled".to_owned() } else { trimmed.chars().take(80).collect() }
}

fn kind_of(path: &Path) -> Option<Kind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "nam" => Some(Kind::Model),
        "wav" => Some(Kind::Ir),
        _ => None,
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&path, out),
            Ok(t) if t.is_file() || t.is_symlink() => out.push(path),
            _ => {}
        }
    }
}

fn stamp(path: &Path) -> (u64, u64) {
    std::fs::metadata(path)
        .map(|m| {
            let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            (mtime, m.len())
        })
        .unwrap_or_default()
}

fn read_entry(path: &Path, kind: Kind, folder: Option<PathBuf>, stamp: (u64, u64)) -> Entry {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let origin: Option<Origin> = std::fs::read(origin_path(path)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let mut entry = Entry {
        path: path.to_owned(),
        kind,
        name: stem,
        author: origin.as_ref().and_then(|o| o.author.clone()),
        gear: origin.as_ref().and_then(|o| (!o.makes.is_empty()).then(|| o.makes.join(", "))),
        gear_type: origin.as_ref().and_then(|o| o.gear.clone()),
        tone_type: None,
        architecture: None,
        sample_rate: None,
        origin,
        folder,
        favourite: false,
        tags: Vec::new(),
        stamp,
        error: None,
    };
    match kind {
        Kind::Model => match scythe_engine::nam::NamInfo::read(path) {
            Ok(info) => {
                if let Some(n) = info.name {
                    entry.name = n;
                }
                entry.author = info.modeled_by.or(entry.author);
                let gear: Vec<String> = [info.gear_make, info.gear_model].into_iter().flatten().collect();
                if !gear.is_empty() {
                    entry.gear = Some(gear.join(" "));
                }
                entry.gear_type = info.gear_type.or(entry.gear_type);
                entry.tone_type = info.tone_type;
                entry.architecture =
                    Some(info.size.map_or(info.architecture.clone(), |s| format!("{} {s}", info.architecture)));
                entry.sample_rate = info.sample_rate;
            }
            Err(e) => entry.error = Some(e.to_string()),
        },
        Kind::Ir => match hound::WavReader::open(path) {
            Ok(r) => {
                let spec = r.spec();
                entry.sample_rate = Some(spec.sample_rate as f64);
                entry.architecture = Some(format!("IR {} ch, {} ms", spec.channels, {
                    let frames = r.duration() as f64;
                    (frames * 1000.0 / spec.sample_rate.max(1) as f64).round()
                }));
            }
            Err(e) => entry.error = Some(e.to_string()),
        },
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scythe-lib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn nam_json(name: &str, by: &str, make: &str, gear_type: &str) -> String {
        format!(
            r#"{{"version": "0.5.4", "architecture": "WaveNet", "sample_rate": 48000,
            "config": {{"layers": [{{"channels": 12}}, {{"channels": 6}}]}}, "weights": [0.0],
            "metadata": {{"name": "{name}", "modeled_by": "{by}", "gear_make": "{make}", "gear_model": "{name}",
              "gear_type": "{gear_type}", "tone_type": "hi_gain", "loudness": -20.0}}}}"#
        )
    }

    fn wav(path: &Path, frames: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for _ in 0..frames {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
    }

    /// A folder laid out like `~/music/Samples/NAM`: models in sub-folders, impulse responses,
    /// a model without metadata and a file that is not a model.
    fn music_folder(root: &Path) {
        std::fs::create_dir_all(root.join("Full stacks")).unwrap();
        std::fs::create_dir_all(root.join("Pedals")).unwrap();
        std::fs::create_dir_all(root.join("IR/Mesa OS 4x12 IR")).unwrap();
        std::fs::write(root.join("Full stacks/6505.nam"), nam_json("6505+", "Some One", "Peavey", "amp")).unwrap();
        std::fs::write(root.join("Pedals/TS9.nam"), nam_json("TS9", "Pedal Person", "Ibanez", "pedal")).unwrap();
        std::fs::write(root.join("Pedals/bare.nam"), r#"{"architecture": "LSTM", "config": {}, "weights": []}"#)
            .unwrap();
        std::fs::write(root.join("Pedals/broken.nam"), "not json").unwrap();
        std::fs::write(root.join("notes.txt"), "hello").unwrap();
        std::fs::write(root.join(".hidden.nam"), nam_json("x", "x", "x", "amp")).unwrap();
        wav(&root.join("IR/Mesa OS 4x12 IR/57.wav"), 4800);
    }

    #[test]
    fn indexes_folders_with_their_metadata() {
        let dir = scratch("index");
        let music = dir.join("music");
        music_folder(&music);
        let mut lib = Library::open(dir.join("lib"));
        assert!(lib.add_folder(music.clone()));
        assert!(!lib.add_folder(music.clone()));
        lib.merge(Library::scan(&lib.roots(), &[]));
        let names: Vec<&str> = lib.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["57", "6505+", "bare", "broken", "TS9"]);
        let amp = lib.entry(&music.join("Full stacks/6505.nam")).unwrap();
        assert_eq!(amp.author.as_deref(), Some("Some One"));
        assert_eq!(amp.gear.as_deref(), Some("Peavey 6505+"));
        assert_eq!(amp.gear_type.as_deref(), Some("amp"));
        assert_eq!(amp.architecture.as_deref(), Some("WaveNet lite"));
        assert_eq!(amp.sample_rate, Some(48000.0));
        assert_eq!(amp.location(), "Full stacks");
        assert_eq!(amp.summary(), "Peavey 6505+ · amp · hi_gain · by Some One");
        let ir = lib.entry(&music.join("IR/Mesa OS 4x12 IR/57.wav")).unwrap();
        assert_eq!((ir.kind, ir.architecture.as_deref()), (Kind::Ir, Some("IR 1 ch, 100 ms")));
        assert!(lib.entry(&music.join("Pedals/broken.nam")).unwrap().error.is_some());
        assert_eq!(lib.entry(&music.join("Pedals/bare.nam")).unwrap().architecture.as_deref(), Some("LSTM"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rescans_reuse_unchanged_files_and_keep_favourites() {
        let dir = scratch("rescan");
        let music = dir.join("music");
        music_folder(&music);
        let mut lib = Library::open(dir.join("lib"));
        lib.add_folder(music.clone());
        lib.merge(Library::scan(&lib.roots(), &[]));
        let ts9 = music.join("Pedals/TS9.nam");
        lib.set_favourite(&ts9, true);
        lib.set_tags(&ts9, vec!["boost ".into(), "".into(), "solo".into()]);
        let mut known = lib.entries().to_vec();
        known.iter_mut().find(|e| e.path == ts9).unwrap().name = "remembered".into();
        std::fs::remove_file(music.join("Pedals/bare.nam")).unwrap();
        lib.merge(Library::scan(&lib.roots(), &known));
        let e = lib.entry(&ts9).unwrap();
        assert_eq!(e.name, "remembered", "an unchanged file is not read again");
        assert!(e.favourite);
        assert_eq!(e.tags, ["boost", "solo"]);
        assert!(lib.entry(&music.join("Pedals/bare.nam")).is_none());
        lib.save().unwrap();
        let reopened = Library::open(dir.join("lib"));
        assert_eq!(reopened.folders, std::slice::from_ref(&music));
        assert!(reopened.entry(&ts9).unwrap().favourite);
        lib.remove_folder(&music);
        assert!(lib.entries().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn search_filters_and_steps_through_models() {
        let dir = scratch("search");
        let music = dir.join("music");
        music_folder(&music);
        let mut lib = Library::open(dir.join("lib"));
        lib.add_folder(music.clone());
        lib.merge(Library::scan(&lib.roots(), &[]));
        let names = |lib: &Library, f: &Filter| lib.search(f).iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&lib, &Filter { text: "peavey".into(), ..Filter::default() }), ["6505+"]);
        assert_eq!(names(&lib, &Filter { text: "pedals ibanez".into(), ..Filter::default() }), ["TS9"]);
        assert_eq!(names(&lib, &Filter { text: "mesa".into(), ..Filter::default() }), ["57"], "folder names count");
        assert_eq!(names(&lib, &Filter { kind: Some(Kind::Ir), ..Filter::default() }), ["57"]);
        let models = Filter { kind: Some(Kind::Model), ..Filter::default() };
        let first = lib.neighbour(None, 1, &models).unwrap().path.clone();
        assert_eq!(lib.entry(&first).unwrap().name, "6505+");
        assert_eq!(lib.neighbour(Some(&first), -1, &models).unwrap().name, "TS9", "wraps around");
        assert_eq!(lib.neighbour(Some(&first), 1, &models).unwrap().name, "bare");
        let ts9 = music.join("Pedals/TS9.nam");
        lib.set_favourite(&ts9, true);
        lib.set_tags(&ts9, vec!["Boost".into()]);
        assert_eq!(names(&lib, &Filter { favourites: true, ..Filter::default() }), ["TS9"]);
        assert_eq!(names(&lib, &Filter { tag: Some("boost".into()), ..Filter::default() }), ["TS9"]);
        assert_eq!(lib.tags(), ["Boost"]);
        assert_eq!(names(&lib, &Filter { place: Some(Place::Downloads), ..Filter::default() }), Vec::<String>::new());
        let folder = Filter { place: Some(Place::Folder(music.clone())), ..Filter::default() };
        assert_eq!(lib.search(&folder).len(), lib.entries().len());
        for p in ["a", "b", "a"] {
            lib.touch(Path::new(p));
        }
        assert_eq!(lib.recent(), [PathBuf::from("a"), PathBuf::from("b")]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn downloads_keep_where_they_came_from() {
        let dir = scratch("download");
        let mut lib = Library::open(dir.join("lib"));
        let tone: Tone = serde_json::from_str(
            r#"{"id": 4711, "title": "Friedman BE-100 / Deluxe", "gear": "amp", "license": "t3k",
                "user": {"id": 1, "username": "tk", "display_name": "Tone King"},
                "makes": [{"id": 3, "name": "Friedman"}], "url": "https://www.tone3000.com/tones/4711"}"#,
        )
        .unwrap();
        let model = Model { id: 9, tone_id: 4711, name: "BE ch".into(), model_url: "u".into(), ..Model::default() };
        let bytes = nam_json("BE-100", "", "Friedman", "amp");
        let entry = lib.store_download(&tone, &model, bytes.as_bytes()).unwrap();
        assert!(entry.path.starts_with(dir.join("lib/tone3000/4711-Friedman BE-100 _ Deluxe")));
        assert_eq!(entry.path.file_name().unwrap(), "BE ch.nam");
        assert_eq!(entry.author.as_deref(), Some("Tone King"));
        assert_eq!(entry.origin.as_ref().unwrap().url.as_deref(), Some("https://www.tone3000.com/tones/4711"));
        assert_eq!(lib.downloaded(9).map(|e| e.path.clone()), Some(entry.path.clone()));
        let ir = Model { id: 10, name: "cab".into(), ..Model::default() };
        let mut wav_bytes = Vec::new();
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 44_100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut wav_bytes), spec).unwrap();
            w.write_sample(1i16).unwrap();
            w.finalize().unwrap();
        }
        assert_eq!(lib.store_download(&tone, &ir, &wav_bytes).unwrap().kind, Kind::Ir);
        let rebuilt = Library::scan(&lib.roots(), &[]);
        let again = rebuilt.iter().find(|e| e.path == entry.path).unwrap();
        assert_eq!(again.origin, entry.origin, "read back from the file beside the model");
        assert_eq!(again.folder, None);
        std::fs::remove_dir_all(dir).ok();
    }
}
