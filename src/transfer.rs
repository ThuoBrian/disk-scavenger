use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

// 256KB buffers — large enough for throughput on external drives, fits in L2 cache
const BUFFER_SIZE: usize = 256 * 1024;

/// Outcome of a batch offload. Partial failures are data, not errors:
/// the batch already ran, and callers must know what did and didn't land.
pub struct OffloadSummary {
    pub copied: usize,
    pub failed: Vec<(PathBuf, String)>,
}

/// Copy `files` under `dest`, keeping their layout relative to the deepest
/// directory they share (so `a/x.mp4` and `b/x.mp4` don't collide).
pub fn offload(files: &[PathBuf], dest: &Path, delete_after: bool) -> Result<OffloadSummary> {
    if !dest.is_dir() {
        anyhow::bail!(
            "Destination does not exist or is not a directory: {}",
            dest.display()
        );
    }

    let plan = plan_destinations(files, dest)?;
    let total_size: u64 = files
        .iter()
        .filter_map(|f| f.metadata().ok())
        .map(|m| m.len())
        .sum();

    eprintln!(
        "Offloading {} file(s) ({}) to {}",
        files.len(),
        crate::human_size(total_size),
        dest.display()
    );
    if delete_after {
        eprintln!("Source files will be trashed after every copy is verified.");
    }

    let progress = ProgressBar::new(files.len() as u64);
    progress.set_style(
        ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan} {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );

    let errors: Vec<(PathBuf, String)> = plan
        .par_iter()
        .filter_map(|(src, dest_path)| {
            let result = copy_file_verified(src, dest_path);
            progress.inc(1);
            result.err().map(|e| (src.clone(), format!("{e:#}")))
        })
        .collect();

    progress.finish_with_message("done");

    let success_count = files.len() - errors.len();
    if !errors.is_empty() {
        eprintln!("\n{} file(s) failed:", errors.len());
        for (path, err) in &errors {
            eprintln!("  {}: {}", path.display(), err);
        }
    }
    eprintln!("\n{success_count} file(s) copied and verified successfully.");

    if delete_after && errors.is_empty() {
        eprintln!("Trashing source files...");
        for src in files {
            if let Err(e) = trash::delete(src) {
                eprintln!("  Failed to trash {}: {}", src.display(), e);
            }
        }
        for note in crate::actions::trash_notes(total_size) {
            eprintln!("{note}");
        }
    } else if delete_after {
        eprintln!("Skipping deletion due to copy errors.");
    }

    Ok(OffloadSummary {
        copied: success_count,
        failed: errors,
    })
}

/// Map each source file to its destination path.
fn plan_destinations(files: &[PathBuf], dest: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let abs: Vec<PathBuf> = files
        .iter()
        .map(|f| std::path::absolute(f).with_context(|| format!("Bad path: {}", f.display())))
        .collect::<Result<_>>()?;
    let root = common_parent(&abs);

    abs.into_iter()
        .zip(files)
        .map(|(a, original)| {
            let rel = match &root {
                Some(r) => a.strip_prefix(r).map(Path::to_path_buf).unwrap_or_default(),
                // Different drives: fall back to the full path, drive letter as a folder
                None => drive_as_folder(&a),
            };
            if rel.as_os_str().is_empty() || rel.components().any(|c| c == Component::ParentDir) {
                anyhow::bail!("Cannot place {} under the destination", original.display());
            }
            Ok((original.clone(), dest.join(rel)))
        })
        .collect()
}

/// Deepest directory containing every path, or `None` if they share no root
/// (e.g. files on two Windows drives).
fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut common = paths.first()?.parent()?.to_path_buf();
    for p in &paths[1..] {
        while !p.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    // `pop` can bottom out at a bare prefix like `C:` — not a usable root
    common.has_root().then_some(common)
}

fn drive_as_folder(path: &Path) -> PathBuf {
    path.components()
        .filter_map(|c| match c {
            Component::Prefix(p) => Some(
                p.as_os_str()
                    .to_string_lossy()
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .collect::<String>()
                    .into(),
            ),
            Component::Normal(n) => Some(PathBuf::from(n)),
            _ => None,
        })
        .collect()
}

fn copy_file_verified(src: &Path, dest_path: &Path) -> Result<()> {
    let src_meta =
        std::fs::metadata(src).with_context(|| format!("Cannot read source: {}", src.display()))?;
    if !src_meta.is_file() {
        anyhow::bail!("Not a regular file: {}", src.display());
    }

    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create folder: {}", parent.display()))?;
    }

    // create_new is atomic: never overwrites, even if two workers race on one name
    let target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest_path)
        .with_context(|| {
            if dest_path.exists() {
                format!("Destination already exists: {}", dest_path.display())
            } else {
                format!("Cannot create destination: {}", dest_path.display())
            }
        })?;

    let outcome = copy_and_verify(src, target, dest_path);
    if outcome.is_err() {
        // We created it, so it's ours to remove — a partial copy would block a retry
        let _ = std::fs::remove_file(dest_path);
    }
    outcome
}

fn copy_and_verify(src: &Path, mut target: std::fs::File, dest_path: &Path) -> Result<()> {
    // Hash the source as it streams, so it's read once instead of twice
    let mut source = std::fs::File::open(src)
        .with_context(|| format!("Cannot open source: {}", src.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; BUFFER_SIZE];
    loop {
        let n = source
            .read(&mut buf)
            .with_context(|| format!("Read error: {}", src.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        target
            .write_all(&buf[..n])
            .with_context(|| format!("Write error: {}", dest_path.display()))?;
    }
    target
        .sync_all()
        .with_context(|| format!("Sync error: {}", dest_path.display()))?;
    drop(target);

    // Re-read the destination after sync to verify what actually landed
    let src_hash = format!("{:x}", hasher.finalize());
    let dest_hash = crate::hash_file(dest_path)?;
    if src_hash != dest_hash {
        anyhow::bail!(
            "Checksum mismatch — corrupt copy deleted: {}",
            src.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copy_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dest = dir.path().join("out/src.txt");
        std::fs::write(&src, b"Hello, disk scavenger!").unwrap();

        copy_file_verified(&src, &dest).expect("copy should succeed");
        assert_eq!(std::fs::read(&dest).unwrap(), b"Hello, disk scavenger!");
    }

    #[test]
    fn test_copy_refuses_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dest = dir.path().join("dest.txt");
        std::fs::write(&src, b"source").unwrap();
        std::fs::write(&dest, b"existing").unwrap();

        assert!(copy_file_verified(&src, &dest).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"existing");
    }

    #[test]
    fn test_copy_nonexistent_source() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest.bin");
        assert!(copy_file_verified(&dir.path().join("nope.bin"), &dest).is_err());
        assert!(!dest.exists(), "no stray file left behind");
    }

    #[test]
    fn test_offload_same_names_keep_their_folders() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(src.join("a")).unwrap();
        std::fs::create_dir_all(src.join("b")).unwrap();
        std::fs::create_dir(&out).unwrap();
        std::fs::write(src.join("a/clip.mp4"), b"first").unwrap();
        std::fs::write(src.join("b/clip.mp4"), b"second").unwrap();

        let files = vec![src.join("a/clip.mp4"), src.join("b/clip.mp4")];
        let s = offload(&files, &out, false).unwrap();
        assert_eq!(s.copied, 2);
        assert_eq!(std::fs::read(out.join("a/clip.mp4")).unwrap(), b"first");
        assert_eq!(std::fs::read(out.join("b/clip.mp4")).unwrap(), b"second");
    }

    #[test]
    fn test_offload_same_file_twice_never_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let f = dir.path().join("x.bin");
        std::fs::write(&f, vec![9u8; 1024 * 1024]).unwrap();

        // Same destination twice in one parallel batch: exactly one may win
        let s = offload(&[f.clone(), f.clone()], &out, false).unwrap();
        assert_eq!(s.copied, 1);
        assert_eq!(s.failed.len(), 1);
        assert_eq!(std::fs::read(out.join("x.bin")).unwrap().len(), 1024 * 1024);
    }

    #[test]
    fn test_single_file_lands_at_top_level() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("deep/er/file.iso");
        let plan = plan_destinations(std::slice::from_ref(&f), Path::new("/dest")).unwrap();
        assert_eq!(plan[0].1, Path::new("/dest").join("file.iso"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_files_on_two_drives() {
        let files = [PathBuf::from(r"C:\a\x.bin"), PathBuf::from(r"D:\b\x.bin")];
        let plan = plan_destinations(&files, Path::new(r"E:\out")).unwrap();
        assert_eq!(plan[0].1, PathBuf::from(r"E:\out\C\a\x.bin"));
        assert_eq!(plan[1].1, PathBuf::from(r"E:\out\D\b\x.bin"));
    }
}
