//! The media explorer's files: REAPER's media databases (`MediaDB/NN.ReaperFileList`), the
//! shortcut list it keeps in reaper.ini (`[reaper_sexplorer]`), the metadata columns and the
//! search.
//!
//! A database as REAPER 7.73 writes it (measured by scanning a folder with REAPER):
//!
//! ```text
//! PATH "/music/loops"
//! FILE "/music/loops/bass.flac" 47817 0 1794261402 1
//! DATA "t:Loop Title" a:Tester p:128 k:Am
//! DATA s:48000 n:2 l:0:03.000 i:16
//! ```
//!
//! `PATH` lines name the folders the database scans. Each `FILE` gives the file's size as two
//! 32-bit words (low, high), its modification time in Unix seconds and flags (bit 0 marks a
//! favourite); the `DATA` lines after it hold its metadata as `key:value` tokens, four to a line.

use std::path::Path;

use crate::ini::Ini;
use crate::rpp::{quote, quote_always, tokenize};

/// The reaper.ini section the media explorer keeps its shortcuts in.
pub const SECTION: &str = "reaper_sexplorer";
/// The folder of the databases, in REAPER's resource folder.
pub const DB_DIR: &str = "MediaDB";
pub const DB_EXTENSION: &str = "ReaperFileList";
/// `FILE` flag: the file is a favourite.
pub const FAVORITE: u32 = 1;
/// Metadata tokens REAPER puts on one `DATA` line.
const TAGS_PER_LINE: usize = 4;

/// Metadata of a media file, as the explorer's columns show it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaInfo {
    /// Seconds.
    pub length: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    /// Bits per sample of PCM files.
    pub bits: Option<u32>,
    /// Kilobits per second of compressed files.
    pub bitrate: Option<u32>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub date: Option<String>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub description: Option<String>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
}

impl MediaInfo {
    /// The metadata in a database's `DATA` tokens, in the order REAPER writes them.
    pub fn tags(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut text = |k: &str, v: &Option<String>| {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                out.push((k.to_owned(), v.to_owned()));
            }
        };
        text("t", &self.title);
        text("a", &self.artist);
        text("b", &self.album);
        text("y", &self.date);
        text("g", &self.genre);
        text("c", &self.comment);
        text("d", &self.description);
        if let Some(bpm) = self.bpm {
            out.push(("p".into(), format_number(bpm)));
        }
        if let Some(k) = self.key.as_deref().filter(|k| !k.is_empty()) {
            out.push(("k".into(), k.to_owned()));
        }
        if let Some(s) = self.sample_rate {
            out.push(("s".into(), s.to_string()));
        }
        if let Some(n) = self.channels {
            out.push(("n".into(), n.to_string()));
        }
        if let Some(l) = self.length {
            out.push(("l".into(), format_length(l)));
        }
        match (self.bits, self.bitrate) {
            (Some(b), _) => out.push(("i".into(), b.to_string())),
            (None, Some(k)) => out.push(("i".into(), format!("{k}k"))),
            _ => {}
        }
        out
    }

    /// Reads the metadata tokens of a database entry; unknown keys are ignored.
    pub fn from_tags(tags: &[(String, String)]) -> Self {
        let mut info = Self::default();
        for (k, v) in tags {
            let text = Some(v.clone());
            match k.as_str() {
                "t" => info.title = text,
                "a" => info.artist = text,
                "b" => info.album = text,
                "y" => info.date = text,
                "g" => info.genre = text,
                "c" => info.comment = text,
                "d" => info.description = text,
                "p" => info.bpm = v.parse().ok().filter(|b: &f64| *b > 0.0),
                "k" => info.key = text,
                "s" => info.sample_rate = v.parse().ok(),
                "n" => info.channels = v.parse().ok(),
                "l" => info.length = parse_length(v),
                "i" => match v.strip_suffix('k') {
                    Some(k) => info.bitrate = k.parse().ok(),
                    None => info.bits = v.parse().ok(),
                },
                _ => {}
            }
        }
        info
    }

    /// The text fields a search looks through.
    pub fn search_fields(&self) -> impl Iterator<Item = &str> {
        [&self.title, &self.artist, &self.album, &self.date, &self.genre, &self.comment, &self.description, &self.key]
            .into_iter()
            .filter_map(|f| f.as_deref())
    }
}

/// `0:03.000`, `12:00.500`, `1:02:03.000`: how REAPER writes a length.
pub fn format_length(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if h > 0 { format!("{h}:{m:02}:{s:02}.{ms:03}") } else { format!("{m}:{s:02}.{ms:03}") }
}

pub fn parse_length(text: &str) -> Option<f64> {
    text.split(':').try_fold(0.0, |acc, part| part.trim().parse::<f64>().ok().map(|v| acc * 60.0 + v))
}

fn format_number(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// One file of a database.
#[derive(Debug, Clone, PartialEq)]
pub struct DbFile {
    pub path: String,
    pub size: u64,
    /// Modification time, Unix seconds.
    pub modified: u64,
    pub flags: u32,
    /// The `DATA` tokens, split at their first `:`.
    pub tags: Vec<(String, String)>,
}

impl DbFile {
    pub fn favorite(&self) -> bool {
        self.flags & FAVORITE != 0
    }

    pub fn info(&self) -> MediaInfo {
        MediaInfo::from_tags(&self.tags)
    }

    /// Replaces the metadata REAPER reads from the file, keeping tokens it does not know.
    pub fn set_info(&mut self, info: &MediaInfo) {
        const KNOWN: &[&str] = &["t", "a", "b", "y", "g", "c", "d", "p", "k", "s", "n", "l", "i"];
        self.tags.retain(|(k, _)| !KNOWN.contains(&k.as_str()));
        let mut tags = info.tags();
        tags.append(&mut self.tags);
        self.tags = tags;
    }
}

/// A media database: the folders it scans and the files it holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaDb {
    pub paths: Vec<String>,
    pub files: Vec<DbFile>,
    /// Lines Scythe does not interpret (`USER` column definitions), kept as they were.
    pub other: Vec<String>,
}

impl MediaDb {
    pub fn parse(text: &str) -> Self {
        let mut db = Self::default();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let toks = tokenize(line);
            match toks.first().map(String::as_str) {
                Some("PATH") if toks.len() > 1 => db.paths.push(toks[1].clone()),
                Some("FILE") if toks.len() > 1 => {
                    let num = |i: usize| toks.get(i).and_then(|t| t.parse::<u64>().ok()).unwrap_or(0);
                    db.files.push(DbFile {
                        path: toks[1].clone(),
                        size: num(2) | (num(3) << 32),
                        modified: num(4),
                        flags: num(5) as u32,
                        tags: Vec::new(),
                    });
                }
                Some("DATA") => {
                    if let Some(f) = db.files.last_mut() {
                        f.tags.extend(toks[1..].iter().map(|t| match t.split_once(':') {
                            Some((k, v)) => (k.to_owned(), v.to_owned()),
                            None => (t.clone(), String::new()),
                        }));
                    }
                }
                _ => db.other.push(line.to_owned()),
            }
        }
        db
    }

    pub fn write(&self) -> String {
        let mut out = String::new();
        for p in &self.paths {
            out.push_str(&format!("PATH {}\n", quote_always(p)));
        }
        for l in &self.other {
            out.push_str(l);
            out.push('\n');
        }
        for f in &self.files {
            out.push_str(&format!(
                "FILE {} {} {} {} {}\n",
                quote_always(&f.path),
                f.size & 0xffff_ffff,
                f.size >> 32,
                f.modified,
                f.flags
            ));
            for chunk in f.tags.chunks(TAGS_PER_LINE) {
                let toks: Vec<String> = chunk.iter().map(|(k, v)| quote(&format!("{k}:{v}"))).collect();
                out.push_str(&format!("DATA {}\n", toks.join(" ")));
            }
        }
        out
    }

    pub fn file(&self, path: &str) -> Option<&DbFile> {
        self.files.iter().find(|f| f.path == path)
    }

    pub fn file_mut(&mut self, path: &str) -> Option<&mut DbFile> {
        self.files.iter_mut().find(|f| f.path == path)
    }
}

/// An entry of the explorer's shortcut list: a folder, or a database file in `MediaDB`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    /// `ShortcutN`: a folder path or a database's file name (`00.ReaperFileList`).
    pub target: String,
    /// `ShortcutTN`: the name shown; REAPER writes it for databases.
    pub title: Option<String>,
}

impl Shortcut {
    pub fn is_database(&self) -> bool {
        is_database(&self.target)
    }

    pub fn name(&self) -> String {
        if let Some(t) = self.title.as_deref().filter(|t| !t.is_empty()) {
            return t.to_owned();
        }
        Path::new(&self.target).file_name().map_or_else(|| self.target.clone(), |n| n.to_string_lossy().into_owned())
    }
}

pub fn is_database(target: &str) -> bool {
    Path::new(target).extension().is_some_and(|e| e.eq_ignore_ascii_case(DB_EXTENSION))
}

/// The shortcut list (`NbShortcuts`, `ShortcutN`, `ShortcutTN`).
pub fn shortcuts(ini: &Ini) -> Vec<Shortcut> {
    let Some(s) = ini.section(SECTION) else { return Vec::new() };
    let count = s.get("NbShortcuts").and_then(|n| n.trim().parse::<usize>().ok()).unwrap_or(0);
    (0..count)
        .filter_map(|i| {
            let target = s.get(&format!("Shortcut{i}"))?.to_owned();
            let title = s.get(&format!("ShortcutT{i}")).map(str::to_owned);
            Some(Shortcut { target, title })
        })
        .collect()
}

/// Writes the shortcut list into `ini`, replacing the old one and keeping the section's other keys.
pub fn set_shortcuts(ini: &mut Ini, list: &[Shortcut]) {
    let s = ini.section_mut(SECTION);
    s.entries.retain(|(k, _)| k != "NbShortcuts" && !k.starts_with("Shortcut"));
    s.set("NbShortcuts", &list.len().to_string());
    for (i, sc) in list.iter().enumerate() {
        s.set(&format!("Shortcut{i}"), &sc.target);
        if let Some(t) = &sc.title {
            s.set(&format!("ShortcutT{i}"), t);
        }
    }
}

/// The file name for a new database: the first free `NN.ReaperFileList`, as REAPER numbers them.
pub fn new_database_name(existing: &[String]) -> String {
    (0..)
        .map(|n| format!("{n:02}.{DB_EXTENSION}"))
        .find(|name| !existing.iter().any(|e| e.eq_ignore_ascii_case(name)))
        .unwrap_or_default()
}

/// Files the explorer lists as media (REAPER's "supported media only").
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "wav", "wave", "bwf", "w64", "aif", "aiff", "aifc", "caf", "flac", "mp3", "mp2", "ogg", "oga", "opus", "wv", "m4a",
    "aac", "mka", "wma", "mid", "midi", "kar", "rmi", "syx", "mp4", "m4v", "mov", "qt", "mkv", "webm", "avi", "mpg",
    "mpeg", "rpp",
];

fn extension(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

pub fn is_media(path: &Path) -> bool {
    extension(path).is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.as_str()))
}

pub fn is_midi(path: &Path) -> bool {
    extension(path).is_some_and(|e| matches!(e.as_str(), "mid" | "midi" | "kar" | "rmi"))
}

/// A search as typed into the explorer: every word must appear (in any order, ignoring case),
/// a word starting with `-` must not, and `"quoted words"` must appear together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Query {
    pub fn parse(text: &str) -> Self {
        let mut q = Self::default();
        let mut rest = text.trim();
        while !rest.is_empty() {
            let negate = rest.starts_with('-') && rest.len() > 1;
            if negate {
                rest = &rest[1..];
            }
            let (term, tail) = match rest.strip_prefix('"') {
                Some(quoted) => match quoted.split_once('"') {
                    Some((t, tail)) => (t, tail),
                    None => (quoted, ""),
                },
                None => rest.split_once(char::is_whitespace).unwrap_or((rest, "")),
            };
            let term = term.trim().to_lowercase();
            if !term.is_empty() {
                if negate { q.exclude.push(term) } else { q.include.push(term) }
            }
            rest = tail.trim_start();
        }
        q
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Whether the texts (file name, metadata) match.
    pub fn matches<'a>(&self, texts: impl IntoIterator<Item = &'a str>) -> bool {
        let hay = texts.into_iter().map(str::to_lowercase).collect::<Vec<_>>().join("\n");
        self.include.iter().all(|t| hay.contains(t.as_str())) && !self.exclude.iter().any(|t| hay.contains(t.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written by REAPER 7.73 scanning a folder: favourites, metadata split over lines, tokens
    /// quoted when they hold a space.
    const REAPER_DB: &str = r#"PATH "/lib"
FILE "/lib/bassloop.flac" 47817 0 1794261402 1
DATA "t:Loop Title" a:Tester p:128 k:Am
DATA s:48000 n:2 l:0:03.000 i:16
FILE "/lib/sub/hit.mp3" 25048 0 1794261402 0
DATA g:Rock p:95 k:C s:48000
DATA n:2 l:0:01.500 i:133k
FILE "/lib/tagged.wav" 768760 0 1794261539 0
DATA "b:My Album" y:2024 g:Ambient "c:A comment here"
DATA "d:BWF desc" s:48000 n:2 l:0:04.000
DATA i:16
FILE "/lib/vorb.ogg" 7023 0 1794261539 0
DATA p:140.5 k:F#m s:44100 n:1
DATA l:0:02.000 i:28k
"#;

    #[test]
    fn reads_a_database_reaper_wrote() {
        let db = MediaDb::parse(REAPER_DB);
        assert_eq!(db.paths, ["/lib"]);
        assert_eq!(db.files.len(), 4);
        let bass = &db.files[0];
        assert!(bass.favorite());
        assert_eq!((bass.size, bass.modified), (47817, 1794261402));
        let info = bass.info();
        assert_eq!(info.title.as_deref(), Some("Loop Title"));
        assert_eq!(info.bpm, Some(128.0));
        assert_eq!(info.key.as_deref(), Some("Am"));
        assert_eq!((info.sample_rate, info.channels, info.bits), (Some(48000), Some(2), Some(16)));
        assert_eq!(info.length, Some(3.0));
        let hit = db.files[1].info();
        assert_eq!((hit.genre.as_deref(), hit.bitrate, hit.bits), (Some("Rock"), Some(133), None));
        let tagged = db.files[2].info();
        assert_eq!(tagged.comment.as_deref(), Some("A comment here"));
        assert_eq!(tagged.description.as_deref(), Some("BWF desc"));
        assert_eq!(db.files[3].info().bpm, Some(140.5));
    }

    #[test]
    fn writes_a_database_as_reaper_does() {
        assert_eq!(MediaDb::parse(REAPER_DB).write(), REAPER_DB);
    }

    #[test]
    fn metadata_tokens_follow_reapers_order() {
        let db = MediaDb::parse(REAPER_DB);
        for f in &db.files {
            let mut copy = f.clone();
            copy.tags.clear();
            copy.set_info(&f.info());
            assert_eq!(copy.tags, f.tags, "{}", f.path);
        }
    }

    #[test]
    fn large_files_split_their_size() {
        let mut db = MediaDb::default();
        db.files.push(DbFile { path: "/a b.wav".into(), size: (3 << 32) | 5, modified: 7, flags: 0, tags: vec![] });
        assert_eq!(db.write(), "FILE \"/a b.wav\" 5 3 7 0\n");
        assert_eq!(MediaDb::parse(&db.write()), db);
    }

    #[test]
    fn lengths() {
        assert_eq!(format_length(3.0), "0:03.000");
        assert_eq!(format_length(754.25), "12:34.250");
        assert_eq!(format_length(3723.0), "1:02:03.000");
        assert_eq!(parse_length("1:02:03.000"), Some(3723.0));
        assert_eq!(parse_length("0:01.500"), Some(1.5));
    }

    #[test]
    fn shortcuts_round_trip_in_reaper_ini() {
        let mut ini = Ini::parse(
            "[reaper_sexplorer]\nlastdir=/home\nNbShortcuts=2\nShortcut0=00.ReaperFileList\nShortcutT0=TestDB\nShortcut1=/music\n",
        );
        let list = shortcuts(&ini);
        assert_eq!(list.len(), 2);
        assert!(list[0].is_database() && !list[1].is_database());
        assert_eq!((list[0].name(), list[1].name()), ("TestDB".into(), "music".into()));
        set_shortcuts(&mut ini, &list[..1]);
        let s = ini.section(SECTION).unwrap();
        assert_eq!(s.get("lastdir"), Some("/home"));
        assert_eq!(s.get("NbShortcuts"), Some("1"));
        assert_eq!(s.get("Shortcut1"), None);
        assert_eq!(new_database_name(&["00.ReaperFileList".into()]), "01.ReaperFileList");
    }

    #[test]
    fn search_terms() {
        let q = Query::parse("kick -snare \"dry room\"");
        assert_eq!(q.include, ["kick", "dry room"]);
        assert_eq!(q.exclude, ["snare"]);
        assert!(q.matches(["Kick 01.wav", "Dry Room"]));
        assert!(!q.matches(["Kick 01.wav", "room dry"]));
        assert!(!q.matches(["kick snare dry room.wav"]));
        assert!(Query::parse("  ").is_empty());
    }
}
