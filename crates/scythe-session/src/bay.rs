//! The project bay's edits: retargeting a media file and cleaning the project folder of files
//! the project does not use, into the trash (REAPER's default, "Send files to trash") or
//! deleted outright.

use std::path::{Path, PathBuf};

use scythe_core::bay;

use crate::Session;

impl Session {
    /// Points every take playing `from` at `to`, as one undo step. Answers the takes changed.
    pub fn retarget_media(&mut self, from: &Path, to: &Path) -> Result<usize, String> {
        if !to.is_file() {
            return Err(format!("no file {}", to.display()));
        }
        let mut changed = 0;
        let mut copy = self.project.clone();
        if bay::retarget(&mut copy, from, to) == 0 {
            return Err(format!("no take plays {}", from.display()));
        }
        self.edit("Retarget media", |p| changed = bay::retarget(p, from, to));
        self.request_media();
        Ok(changed)
    }

    /// The files in the project's recording folder it does not use.
    pub fn unused_project_files(&self) -> Result<bay::Unused, String> {
        let dir = bay::clean_dir(&self.project).ok_or("save the project first")?;
        bay::unused_files(&self.project, &dir).map_err(|e| format!("{}: {e}", dir.display()))
    }

    /// Removes `files`, which must be among the unused files, to the trash or for good.
    /// Answers the files removed.
    pub fn clean_project_files(&mut self, files: &[PathBuf], trash: bool) -> Result<Vec<PathBuf>, String> {
        let unused = self.unused_project_files()?;
        if let Some(f) = files.iter().find(|f| !unused.files.contains(f)) {
            return Err(format!("{} is not an unused file of this project", f.display()));
        }
        let mut done = Vec::new();
        for f in files {
            let result = if trash { move_to_trash(f) } else { std::fs::remove_file(f).map_err(|e| e.to_string()) };
            match result {
                Ok(()) => done.push(f.clone()),
                Err(e) => self.notify(format!("Cannot remove {}: {e}", f.display())),
            }
        }
        Ok(done)
    }
}

/// Moves a file into the freedesktop trash (`$XDG_DATA_HOME/Trash`), with the `.trashinfo`
/// file managers use to put it back.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or("no home folder for the trash")?
        .join("Trash");
    let (files, info) = (base.join("files"), base.join("info"));
    std::fs::create_dir_all(&files).and_then(|_| std::fs::create_dir_all(&info)).map_err(|e| e.to_string())?;
    let name = path.file_name().ok_or("not a file")?.to_string_lossy().into_owned();
    let target = (0..)
        .map(|n| if n == 0 { name.clone() } else { format!("{name}.{n}") })
        .find(|n| !files.join(n).exists() && !info.join(format!("{n}.trashinfo")).exists())
        .unwrap_or(name);
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let date = time_stamp();
    std::fs::write(
        info.join(format!("{target}.trashinfo")),
        format!("[Trash Info]\nPath={}\nDeletionDate={date}\n", absolute.display()),
    )
    .map_err(|e| e.to_string())?;
    if std::fs::rename(path, files.join(&target)).is_err() {
        std::fs::copy(path, files.join(&target)).and_then(|_| std::fs::remove_file(path)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Now as `YYYY-MM-DDThh:mm:ss` (UTC).
fn time_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}", rest / 3600, rest / 60 % 60, rest % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_of_the_epoch_and_later() {
        let s = time_stamp();
        assert_eq!(s.len(), 19);
        assert!(s.starts_with("20"), "{s}");
    }
}
