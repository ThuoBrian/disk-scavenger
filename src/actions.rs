//! What the frontends do with a selection: clean (trash) or pick files to offload.
//! Shared by the CLI, TUI and GUI so every path gets the same safety checks.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::scanner::dir_size;

/// One actionable row from a scan, or one path given on the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Act on this file or directory as-is
    Path(PathBuf),
    /// Empty this cache directory but keep the directory itself, so the
    /// owning app doesn't trip over a missing folder
    CacheContents(PathBuf),
    /// Trash `redundant` copies, but only after re-checking at action time that
    /// `keep` still exists and every copy still matches it byte for byte
    Duplicate {
        keep: PathBuf,
        redundant: Vec<PathBuf>,
    },
}

impl Target {
    /// Paths that identify this target — used to tie reported problems back to rows.
    pub fn roots(&self) -> Vec<&Path> {
        match self {
            Target::Path(p) | Target::CacheContents(p) => vec![p],
            Target::Duplicate { keep, redundant } => std::iter::once(keep)
                .chain(redundant)
                .map(PathBuf::as_path)
                .collect(),
        }
    }

    /// Bytes this target would free (redundant copies only, for duplicates).
    pub fn size(&self) -> u64 {
        match self {
            Target::Path(p) | Target::CacheContents(p) => dir_size(p),
            Target::Duplicate { redundant, .. } => redundant
                .iter()
                .filter_map(|p| p.metadata().ok())
                .map(|m| m.len())
                .sum(),
        }
    }

    /// Human-readable lines saying exactly what acting on this target touches.
    pub fn describe(&self) -> Vec<String> {
        match self {
            Target::Path(p) => vec![p.display().to_string()],
            Target::CacheContents(p) => vec![format!("contents of {}", p.display())],
            Target::Duplicate { keep, redundant } => {
                std::iter::once(format!("keep   {}", keep.display()))
                    .chain(redundant.iter().map(|p| format!("remove {}", p.display())))
                    .collect()
            }
        }
    }
}

/// Result of a clean. Problems are per path; one bad file never blocks the rest.
#[derive(Default, Debug)]
pub struct CleanSummary {
    pub trashed: usize,
    pub bytes: u64,
    pub problems: Vec<(PathBuf, String)>,
}

impl CleanSummary {
    pub fn report(&self) -> String {
        let mut s = format!(
            "Moved {} item(s) ({}) to the {}.",
            self.trashed,
            crate::human_size(self.bytes),
            TRASH_NAME
        );
        if !self.problems.is_empty() {
            let _ = write!(s, " {} item(s) skipped or failed.", self.problems.len());
        }
        s
    }
}

#[cfg(target_os = "windows")]
pub const TRASH_NAME: &str = "Recycle Bin";
#[cfg(not(target_os = "windows"))]
pub const TRASH_NAME: &str = "Trash";

/// Caveats to show before and after cleaning `bytes`.
pub fn trash_notes(bytes: u64) -> Vec<String> {
    let mut notes = vec![format!(
        "Disk space is only freed once you empty the {TRASH_NAME}."
    )];
    // trash 4.x passes FOF_NO_UI to the shell, so an item too big for the
    // Recycle Bin can be deleted permanently without a prompt.
    if cfg!(target_os = "windows") && bytes > LARGE_TRASH_BYTES {
        notes.push(
            "Windows may permanently delete items too large for the Recycle Bin. \
             Offload anything you might want back first."
                .to_string(),
        );
    }
    notes
}

const LARGE_TRASH_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Move targets to the system trash.
pub fn clean(targets: &[Target]) -> CleanSummary {
    clean_with(targets, |paths| {
        trash::delete_all(paths).map_err(|e| e.to_string())
    })
}

/// `clean` with the delete step injected, so tests don't fill the real trash.
fn clean_with(
    targets: &[Target],
    delete: impl Fn(&[PathBuf]) -> Result<(), String>,
) -> CleanSummary {
    let mut summary = CleanSummary::default();
    let mut queue: Vec<(PathBuf, u64)> = Vec::new();

    for target in targets {
        match target {
            Target::Path(p) => {
                if p.symlink_metadata().is_ok() {
                    queue.push((p.clone(), dir_size(p)));
                } else {
                    summary
                        .problems
                        .push((p.clone(), "no longer exists".into()));
                }
            }
            Target::CacheContents(dir) => match std::fs::read_dir(dir) {
                Ok(entries) => {
                    queue.extend(entries.filter_map(Result::ok).map(|e| e.path()).map(|p| {
                        let size = dir_size(&p);
                        (p, size)
                    }));
                }
                Err(e) => summary.problems.push((dir.clone(), e.to_string())),
            },
            Target::Duplicate { keep, redundant } => {
                verify_duplicates(keep, redundant, &mut queue, &mut summary.problems);
            }
        }
    }

    if queue.is_empty() {
        return summary;
    }
    let paths: Vec<PathBuf> = queue.iter().map(|(p, _)| p.clone()).collect();
    if delete(&paths).is_ok() {
        summary.trashed = queue.len();
        summary.bytes = queue.iter().map(|(_, s)| s).sum();
        return summary;
    }
    // The batch failed (often one locked file under Temp). Retry one by one so
    // the rest still go, and so each failure is reported against its own path.
    for (p, size) in queue {
        if p.symlink_metadata().is_err() {
            // Already went in the failed batch
            summary.trashed += 1;
            summary.bytes += size;
            continue;
        }
        match delete(std::slice::from_ref(&p)) {
            Ok(()) => {
                summary.trashed += 1;
                summary.bytes += size;
            }
            Err(e) => summary.problems.push((p, e)),
        }
    }
    summary
}

/// Queue redundant copies that still match `keep`; report the rest.
fn verify_duplicates(
    keep: &Path,
    redundant: &[PathBuf],
    queue: &mut Vec<(PathBuf, u64)>,
    problems: &mut Vec<(PathBuf, String)>,
) {
    let Ok(keep_hash) = crate::hash_file(keep) else {
        // Without the kept copy, the "redundant" ones are the only copies left
        for r in redundant {
            problems.push((
                r.clone(),
                format!(
                    "kept copy {} is missing or unreadable — not removed",
                    keep.display()
                ),
            ));
        }
        return;
    };
    for r in redundant {
        if same_file::is_same_file(keep, r).unwrap_or(false) {
            problems.push((
                r.clone(),
                "same file as the kept copy (link) — not removed".into(),
            ));
            continue;
        }
        match crate::hash_file(r) {
            Ok(h) if h == keep_hash => {
                let size = r.metadata().map_or(0, |m| m.len());
                queue.push((r.clone(), size));
            }
            Ok(_) => problems.push((r.clone(), "changed since the scan — not removed".into())),
            Err(e) => problems.push((r.clone(), format!("{e:#}"))),
        }
    }
}

/// Split targets into regular files that can be offloaded, and skipped items with reasons.
pub fn offload_files(targets: &[Target]) -> (Vec<PathBuf>, Vec<(PathBuf, String)>) {
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for t in targets {
        match t {
            Target::Path(p) if p.is_file() => files.push(p.clone()),
            Target::Path(p) => skipped.push((
                p.clone(),
                "not a regular file — only files are offloaded".into(),
            )),
            Target::CacheContents(p) => skipped.push((
                p.clone(),
                "caches aren't offloaded — clean them instead".into(),
            )),
            Target::Duplicate { redundant, .. } => files.extend(redundant.iter().cloned()),
        }
    }
    (files, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for the trash: plain delete inside the test's temp dir.
    fn remove(paths: &[PathBuf]) -> Result<(), String> {
        for p in paths {
            let r = if p.is_dir() {
                std::fs::remove_dir_all(p)
            } else {
                std::fs::remove_file(p)
            };
            r.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn dup_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let keep = dir.path().join("keep.bin");
        let extra = dir.path().join("extra.bin");
        std::fs::write(&keep, b"same bytes").unwrap();
        std::fs::write(&extra, b"same bytes").unwrap();
        (dir, keep, extra)
    }

    #[test]
    fn test_duplicate_removed_when_still_identical() {
        let (_dir, keep, extra) = dup_fixture();
        let t = Target::Duplicate {
            keep: keep.clone(),
            redundant: vec![extra.clone()],
        };
        let s = clean_with(&[t], remove);
        assert_eq!(s.trashed, 1);
        assert!(s.problems.is_empty());
        assert!(keep.exists());
        assert!(!extra.exists());
    }

    #[test]
    fn test_duplicate_kept_when_kept_copy_is_gone() {
        let (_dir, keep, extra) = dup_fixture();
        std::fs::remove_file(&keep).unwrap();
        let t = Target::Duplicate {
            keep,
            redundant: vec![extra.clone()],
        };
        let s = clean_with(&[t], remove);
        assert_eq!(s.trashed, 0);
        assert_eq!(s.problems.len(), 1);
        assert!(extra.exists(), "last remaining copy must survive");
    }

    #[test]
    fn test_duplicate_kept_when_changed_since_scan() {
        let (_dir, keep, extra) = dup_fixture();
        std::fs::write(&extra, b"edited bytes").unwrap();
        let t = Target::Duplicate {
            keep,
            redundant: vec![extra.clone()],
        };
        let s = clean_with(&[t], remove);
        assert_eq!(s.trashed, 0);
        assert!(extra.exists());
    }

    #[test]
    fn test_cache_contents_emptied_but_dir_kept() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(cache.join("nested")).unwrap();
        std::fs::write(cache.join("a"), b"1234").unwrap();
        std::fs::write(cache.join("nested/b"), b"56").unwrap();

        let s = clean_with(&[Target::CacheContents(cache.clone())], remove);
        assert_eq!(s.trashed, 2, "two top-level entries");
        assert_eq!(s.bytes, 6);
        assert!(cache.is_dir());
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0);
    }

    #[test]
    fn test_one_failure_does_not_block_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok");
        let locked = dir.path().join("locked");
        std::fs::write(&ok, b"x").unwrap();
        std::fs::write(&locked, b"x").unwrap();
        let locked2 = locked.clone();
        let flaky = move |paths: &[PathBuf]| {
            if paths.contains(&locked2) {
                Err("in use".to_string())
            } else {
                remove(paths)
            }
        };
        let s = clean_with(
            &[Target::Path(ok.clone()), Target::Path(locked.clone())],
            flaky,
        );
        assert_eq!(s.trashed, 1);
        assert_eq!(s.problems, vec![(locked, "in use".to_string())]);
        assert!(!ok.exists());
    }

    #[test]
    fn test_offload_files_skips_dirs_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        let (files, skipped) = offload_files(&[
            Target::Path(f.clone()),
            Target::Path(dir.path().to_path_buf()),
            Target::CacheContents(dir.path().to_path_buf()),
        ]);
        assert_eq!(files, vec![f]);
        assert_eq!(skipped.len(), 2);
    }
}
