//! REAPER's render queue: "Add to render queue" saves a copy of the project, with the files it
//! will write listed at the top, in the `QueuedRenders` folder of the resource directory as
//! `qrender_<yymmdd>_<hhmmss>_<project>.RPP`; "Render all queued renders" renders each copy and
//! deletes it, leaving the ones that fail.

use std::path::{Path, PathBuf};

use crate::model::Project;
use crate::render_settings::{RenderJob, RenderSource};
use crate::rpp::{Node, tokenize};

/// `QueuedRenders` in REAPER's resource directory (`~/.config/REAPER` on Linux).
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("REAPER").join("QueuedRenders"))
}

/// The queued projects in `dir`, oldest first (their names start with the time they were
/// queued).
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            name.starts_with("qrender_") && name.ends_with(".rpp")
        })
        .collect();
    files.sort();
    files
}

/// The queue copy of `p`: the project as `rpp` (its saved text), with a
/// `QUEUED_RENDER_OUTFILE` line per job and the original file name after the header.
pub fn queued_text(p: &Project, rpp: &str, jobs: &[RenderJob]) -> String {
    let mut head = String::new();
    for job in jobs {
        let guid = match job.source {
            RenderSource::Master => p.master.guid.clone(),
            RenderSource::Stem(t) | RenderSource::TrackViaMaster(t) => {
                p.track(t).map(|t| t.guid.clone()).unwrap_or_default()
            }
            RenderSource::Item(i) | RenderSource::ItemViaMaster(i) => {
                p.find_item(i).and_then(|(t, _)| p.track(t)).map(|t| t.guid.clone()).unwrap_or_default()
            }
        };
        head.push_str(&format!(
            "  QUEUED_RENDER_OUTFILE {} 4 {guid}\n",
            crate::rpp::join_tokens([job.path.to_string_lossy()])
        ));
    }
    if let Some(path) = &p.path {
        head.push_str(&format!("  QUEUED_RENDER_ORIGINAL_FILENAME {}\n", path.display()));
    }
    match rpp.find('\n') {
        Some(at) => format!("{}{head}{}", &rpp[..=at], &rpp[at + 1..]),
        None => rpp.to_owned(),
    }
}

/// The file name a project queued at `stamp` (`yymmdd_hhmmss`) gets.
pub fn file_name(p: &Project, stamp: &str) -> String {
    format!("qrender_{stamp}_{}.RPP", p.name())
}

/// The original project a queued copy was made from.
pub fn original(p: &Project) -> Option<PathBuf> {
    lines(p, "QUEUED_RENDER_ORIGINAL_FILENAME").next().and_then(|t| t.get(1).map(PathBuf::from))
}

/// Gives the jobs of a queued copy the file names fixed when it was queued (date wildcards
/// expand then, not when the queue runs): each job takes the first unused listed file of its
/// track, or of the master.
pub fn apply_queued_paths(p: &Project, jobs: &mut [RenderJob]) {
    let mut listed: Vec<Option<(PathBuf, String)>> = lines(p, "QUEUED_RENDER_OUTFILE")
        .filter_map(|t| Some((PathBuf::from(t.get(1)?), t.get(3).cloned().unwrap_or_default())))
        .map(Some)
        .collect();
    let is_track = |guid: &str| p.tracks.iter().any(|t| t.guid.eq_ignore_ascii_case(guid));
    for job in jobs {
        let wanted = match job.source {
            RenderSource::Master => None,
            RenderSource::Stem(t) | RenderSource::TrackViaMaster(t) => p.track(t).map(|t| t.guid.clone()),
            RenderSource::Item(i) | RenderSource::ItemViaMaster(i) => {
                p.find_item(i).and_then(|(t, _)| p.track(t)).map(|t| t.guid.clone())
            }
        };
        let slot = listed.iter_mut().find(|l| {
            l.as_ref().is_some_and(|(_, guid)| match &wanted {
                Some(w) => guid.eq_ignore_ascii_case(w),
                None => !is_track(guid),
            })
        });
        if let Some(slot) = slot
            && let Some((path, _)) = slot.take()
        {
            job.path = path;
        }
    }
}

fn lines<'a>(p: &'a Project, key: &'a str) -> impl Iterator<Item = Vec<String>> + 'a {
    p.extra.iter().filter_map(move |n| match n {
        Node::Line(l) => {
            let t = tokenize(l);
            (t.first().map(String::as_str) == Some(key)).then_some(t)
        }
        Node::Chunk(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_settings::{RenderSelection, RenderSettings};
    use crate::rpp::{project_from_rpp, project_to_rpp};

    const PROJECT: &str = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  RENDER_FILE /out\n  RENDER_PATTERN \"$track-$year\"\n  RENDER_STEMS 1\n  <TRACK {AB}\n    NAME Kick\n    SEL 1\n    <ITEM\n      POSITION 0\n      LENGTH 1\n      <SOURCE WAVE\n        FILE x.wav\n      >\n    >\n  >\n>\n";

    #[test]
    fn a_queued_copy_renders_to_the_names_it_was_queued_with() {
        let mut p = project_from_rpp(PROJECT).unwrap();
        p.path = Some("/songs/a.RPP".into());
        let settings = RenderSettings::from_project(&p);
        let jobs = settings.jobs(&p, &RenderSelection::saved(&p), 0).unwrap();
        assert_eq!(jobs[0].path, PathBuf::from("/out/Kick-1970.wav"));
        let text = queued_text(&p, &project_to_rpp(&p), &jobs);
        let outfile = format!("  QUEUED_RENDER_OUTFILE {} 4 {{AB}}", jobs[0].path.display());
        assert!(text.lines().nth(1).unwrap().starts_with(&outfile));
        assert_eq!(file_name(&p, "261109_095101"), "qrender_261109_095101_a.RPP");

        let q = project_from_rpp(&text).unwrap();
        assert_eq!(original(&q), Some(PathBuf::from("/songs/a.RPP")));
        let mut later = settings.jobs(&q, &RenderSelection::saved(&q), 2_000_000_000).unwrap();
        assert_eq!(later[0].path, PathBuf::from("/out/Kick-2033.wav"));
        apply_queued_paths(&q, &mut later);
        assert_eq!(
            later.iter().map(|j| j.path.clone()).collect::<Vec<_>>(),
            jobs.iter().map(|j| j.path.clone()).collect::<Vec<_>>()
        );
    }
}
