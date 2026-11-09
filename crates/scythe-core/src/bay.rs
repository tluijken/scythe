//! The project bay (REAPER's View: Project bay): the media files, items and effects a
//! project uses, retargeting a file to another one, and the files in the project's folder it
//! does not use (File: Clean current project directory).
//!
//! What "clean project directory" lists was measured with REAPER 7.73 on a scratch project:
//! only the folder recordings go to (`RECORD_PATH`, the project folder when empty), not its
//! subfolders; only files with a media extension from [`CLEAN_EXTENSIONS`] (`.txt`, `.png`,
//! `.jpg`, `.lua`, `.rpp`, `.edl`, `.aac`, `.flv` and `.rx2` are never listed); a peaks file
//! (`x.wav.reapeaks`) is listed unless the file it belongs to is used.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::model::{ItemId, Project, Source, Take, TrackId};
use crate::rpp::{project_to_rpp, resolve_media_path, tokenize};

/// Extensions "clean project directory" considers, as REAPER 7.73 lists them.
pub const CLEAN_EXTENSIONS: &[&str] = &[
    "wav",
    "bwf",
    "w64",
    "aif",
    "aiff",
    "caf",
    "sd2",
    "raw",
    "pcm",
    "flac",
    "mp3",
    "mp2",
    "mpg",
    "ogg",
    "opus",
    "wv",
    "m4a",
    "mka",
    "mkv",
    "mov",
    "mp4",
    "m4v",
    "qt",
    "avi",
    "webm",
    "wma",
    "gif",
    "mid",
    "midi",
    "kar",
    "rmi",
    "syx",
    "rpp-prox",
    "reapindex",
    "reapeaks",
];

/// The file a take plays: its source's file, or the `FILE` line of a source Scythe keeps
/// verbatim (MIDI files, video).
pub fn take_file(take: &Take) -> Option<PathBuf> {
    match &take.source {
        Source::Raw(chunk) => chunk.find_line("FILE").and_then(|t| t.first().cloned()).map(PathBuf::from),
        Source::Section(s) => match &*s.inner {
            Source::Raw(chunk) => chunk.find_line("FILE").and_then(|t| t.first().cloned()).map(PathBuf::from),
            other => other.file().cloned(),
        },
        other => other.file().cloned(),
    }
}

fn set_take_file(take: &mut Take, file: &Path) {
    fn set(source: &mut Source, file: &Path) {
        match source {
            Source::File(p) => *p = file.to_owned(),
            Source::Section(s) => set(&mut s.inner, file),
            Source::Raw(chunk) => {
                for node in &mut chunk.children {
                    if let crate::rpp::Node::Line(l) = node
                        && crate::rpp::line_key(l) == "FILE"
                    {
                        let rest: Vec<String> = tokenize(l).into_iter().skip(2).collect();
                        let mut line = format!("FILE {}", crate::rpp::quote_always(&file.to_string_lossy()));
                        for t in rest {
                            line.push(' ');
                            line.push_str(&crate::rpp::quote(&t));
                        }
                        *l = line;
                    }
                }
            }
            Source::Empty => {}
        }
    }
    set(&mut take.source, file);
}

/// A media file the project uses and where.
#[derive(Debug, Clone, PartialEq)]
pub struct BaySource {
    /// As the project names it (relative to its folder or absolute).
    pub file: PathBuf,
    /// Where it is on disk.
    pub path: PathBuf,
    pub exists: bool,
    /// Takes playing it.
    pub takes: usize,
    pub items: Vec<ItemId>,
    pub tracks: Vec<TrackId>,
}

/// The project's media files in the order they first appear.
pub fn sources(project: &Project) -> Vec<BaySource> {
    let mut out: Vec<BaySource> = Vec::new();
    for track in &project.tracks {
        for item in &track.items {
            for take in &item.takes {
                let Some(file) = take_file(take) else { continue };
                let path = resolve_media_path(project, &file);
                let i = match out.iter().position(|s| s.path == path) {
                    Some(i) => i,
                    None => {
                        let exists = path.exists();
                        out.push(BaySource { file, path, exists, takes: 0, items: Vec::new(), tracks: Vec::new() });
                        out.len() - 1
                    }
                };
                let s = &mut out[i];
                s.takes += 1;
                if !s.items.contains(&item.id) {
                    s.items.push(item.id);
                }
                if !s.tracks.contains(&track.id) {
                    s.tracks.push(track.id);
                }
            }
        }
    }
    out
}

/// An effect type the project uses.
#[derive(Debug, Clone, PartialEq)]
pub struct BayFx {
    pub name: String,
    pub instances: usize,
    /// Track names, the master as "MASTER".
    pub tracks: Vec<String>,
}

pub fn effects(project: &Project) -> Vec<BayFx> {
    let mut out: Vec<BayFx> = Vec::new();
    let tracks = std::iter::once((&project.master, "MASTER".to_owned())).chain(
        project
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t, if t.name.is_empty() { format!("Track {}", i + 1) } else { t.name.clone() })),
    );
    for (track, name) in tracks {
        for fx in &track.fx {
            match out.iter_mut().find(|f| f.name == fx.name) {
                Some(f) => {
                    f.instances += 1;
                    if !f.tracks.contains(&name) {
                        f.tracks.push(name.clone());
                    }
                }
                None => out.push(BayFx { name: fx.name.clone(), instances: 1, tracks: vec![name.clone()] }),
            }
        }
    }
    out
}

/// Points every take playing `from` at `to` instead (REAPER's "replace/retarget media"). A file
/// inside the project's folder is stored relative to it. Answers the takes changed.
pub fn retarget(project: &mut Project, from: &Path, to: &Path) -> usize {
    let dir = project.path.as_deref().and_then(Path::parent).map(Path::to_owned);
    let stored = match &dir {
        Some(d) => to.strip_prefix(d).map_or_else(|_| to.to_owned(), Path::to_owned),
        None => to.to_owned(),
    };
    let from = resolve_media_path(project, from);
    let mut changed = 0;
    let snapshot = project.clone();
    for take in project.tracks.iter_mut().flat_map(|t| &mut t.items).flat_map(|i| &mut i.takes) {
        if take_file(take).is_some_and(|f| resolve_media_path(&snapshot, &f) == from) {
            set_take_file(take, &stored);
            changed += 1;
        }
    }
    changed
}

/// Every file the project refers to with a `FILE` line (takes, frozen tracks, video), resolved.
pub fn used_files(project: &Project) -> HashSet<PathBuf> {
    project_to_rpp(project)
        .lines()
        .filter(|l| crate::rpp::line_key(l) == "FILE")
        .filter_map(|l| tokenize(l).get(1).cloned())
        .map(|f| normalize(&resolve_media_path(project, Path::new(&f))))
        .collect()
}

fn normalize(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_owned())
}

/// The folder "clean project directory" looks in: where recordings go.
pub fn clean_dir(project: &Project) -> Option<PathBuf> {
    let dir = project.path.as_deref()?.parent()?;
    let record = crate::recording::record_path(project);
    Some(if record.is_empty() { dir.to_owned() } else { dir.join(record) })
}

/// The result of looking through a folder for files the project does not use.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Unused {
    pub dir: PathBuf,
    /// Files in the folder, of any kind.
    pub total: usize,
    /// The unused media and peaks files, sorted by name.
    pub files: Vec<PathBuf>,
}

/// The media files in `dir` (not its subfolders) the project does not use, as REAPER's "clean
/// current project directory" lists them.
pub fn unused_files(project: &Project, dir: &Path) -> std::io::Result<Unused> {
    let used = used_files(project);
    let mut out = Unused { dir: dir.to_owned(), ..Default::default() };
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        out.total += 1;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        let Some(ext) = Path::new(&lower).extension().map(|e| e.to_string_lossy().into_owned()) else { continue };
        if !CLEAN_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let owner = match ext.as_str() {
            "reapeaks" | "reapindex" => path.with_file_name(&name[..name.len() - ext.len() - 1]),
            _ => path.clone(),
        };
        if !used.contains(&normalize(&owner)) {
            out.files.push(path);
        }
    }
    out.files.sort_by_key(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        (name.to_lowercase(), name)
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Item;

    fn project_in(dir: &Path) -> Project {
        let text = format!(
            "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 1\n  RECORD_PATH \"\" \"\"\n  <TRACK\n    NAME t1\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      <SOURCE WAVE\n        FILE \"used.wav\"\n      >\n    >\n    <ITEM\n      POSITION 2\n      LENGTH 2\n      <SOURCE WAVE\n        FILE \"{}\"\n      >\n    >\n  >\n>\n",
            dir.join("Media/used_in_media.wav").display()
        );
        let mut p = crate::rpp::project_from_rpp(&text).unwrap();
        p.path = Some(dir.join("proj.RPP"));
        p
    }

    /// The scratch project REAPER 7.73's "clean current project directory" was run on.
    #[test]
    fn lists_unused_files_as_reaper_does() {
        let dir = std::env::temp_dir().join(format!("scythe-bay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Media")).unwrap();
        let names = [
            "used.wav",
            "used.wav.reapeaks",
            "unused.wav",
            "unused.wav.reapeaks",
            "orphan.wav.reapeaks",
            "rendered.wav",
            "unused.aif",
            "unused.flac",
            "unused.mid",
            "unused.mp3",
            "unused.ogg",
            "notes.txt",
            "proj.RPP",
            "proj.RPP-bak",
            "proj.RPP-UNDO",
            "other.RPP",
            "z.png",
            "z.jpg",
            "z.lua",
            "z.rpp-prox",
            "z.RPP-PROX",
            "z.wv",
            "z.opus",
            "z.aac",
            "z.rx2",
            "z.txt.reapeaks",
            "Media/used_in_media.wav",
            "Media/unused2.wav",
        ];
        for n in names {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let p = project_in(&dir);
        let unused = unused_files(&p, &clean_dir(&p).unwrap()).unwrap();
        let listed: Vec<String> =
            unused.files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(
            listed,
            [
                "orphan.wav.reapeaks",
                "rendered.wav",
                "unused.aif",
                "unused.flac",
                "unused.mid",
                "unused.mp3",
                "unused.ogg",
                "unused.wav",
                "unused.wav.reapeaks",
                "z.opus",
                "z.RPP-PROX",
                "z.rpp-prox",
                "z.txt.reapeaks",
                "z.wv",
            ]
        );
        assert_eq!(unused.total, names.len() - 2);
        let media = unused_files(&p, &dir.join("Media")).unwrap();
        assert_eq!(media.total, 2);
        assert_eq!(media.files, [dir.join("Media/unused2.wav")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sources_retarget_and_effects() {
        let dir = Path::new("/projects/song");
        let mut p = project_in(dir);
        let item = Item::new(4.0, 1.0, Take::from_file(PathBuf::from("used.wav")));
        p.tracks[0].items.push(item);
        p.tracks[0].fx.push(crate::FxSlot { name: "VST: ReaEQ (Cockos)".into(), enabled: true, raw: vec![] });
        p.master.fx.push(crate::FxSlot { name: "VST: ReaEQ (Cockos)".into(), enabled: true, raw: vec![] });
        let s = sources(&p);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].file.as_path(), s[0].takes, s[0].items.len()), (Path::new("used.wav"), 2, 2));
        assert_eq!(s[0].path, dir.join("used.wav"));
        assert_eq!(retarget(&mut p, Path::new("/projects/song/used.wav"), &dir.join("Media/new.wav")), 2);
        let s = sources(&p);
        assert_eq!(s[0].file, Path::new("Media/new.wav"));
        assert_eq!(s[0].takes, 2);
        assert_eq!(retarget(&mut p, Path::new("missing.wav"), Path::new("/x.wav")), 0);
        let fx = effects(&p);
        assert_eq!(fx.len(), 1);
        assert_eq!((fx[0].instances, fx[0].tracks.clone()), (2, vec!["MASTER".to_owned(), "t1".to_owned()]));
    }
}
