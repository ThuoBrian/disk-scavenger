use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

use crate::actions::Target;
use crate::hash_file;

const SCAN_DEPTH: usize = 25;
/// Apps smaller than this aren't worth listing
const HEAVY_APP_BYTES: u64 = 500 * 1024 * 1024;
/// Bytes hashed from each end of a file to pre-filter duplicate candidates
const PARTIAL_CHUNK: u64 = 64 * 1024;

/// System directories to skip during scans (performance + relevance)
#[cfg(not(target_os = "windows"))]
fn should_skip(path: &Path) -> bool {
    const SKIP_PREFIXES: &[&str] = &[
        "/dev",
        "/proc",
        "/sys",
        "/run",
        "/snap",
        "/System",
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/private/var/vm",
        "/private/var/db/dyld",
        "/.Spotlight-V100",
        "/.fseventsd",
    ];
    // Component-wise, so `/lib` doesn't also swallow `/library-backup`
    SKIP_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

/// Windows system locations, matched case-insensitively (NTFS is case-insensitive).
/// Drive-level entries only match directly under a drive root, so a user folder
/// named `windows` deep in a project tree is still scanned.
#[cfg(target_os = "windows")]
fn should_skip(path: &Path) -> bool {
    use std::path::Component;

    const ROOT_LEVEL: &[&str] = &[
        "windows",
        "windows.old",
        "$recycle.bin",
        "$windows.~bt",
        "system volume information",
        "recovery",
        "config.msi",
        "msocache",
        "pagefile.sys",
        "hiberfil.sys",
        "swapfile.sys",
    ];
    let lower = |c: Component| match c {
        Component::Normal(os) => Some(os.to_string_lossy().to_lowercase()),
        _ => None,
    };
    let mut normals = path.components().filter_map(lower);
    if path.has_root()
        && normals
            .next()
            .is_some_and(|top| ROOT_LEVEL.contains(&top.as_str()))
    {
        return true;
    }
    // Store-app packages live under Program Files\WindowsApps; locked down anyway
    path.components()
        .filter_map(lower)
        .any(|c| c == "windowsapps")
}

/// What a scan finding is. Also the `--category` filter for `offload`/`clean`.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Personal files over the size threshold
    #[value(name = "large")]
    LargeFile,
    /// Known cache/temp directories
    #[value(name = "caches")]
    Cache,
    /// Redundant copies of identical files (the first copy is always kept)
    #[value(name = "duplicates")]
    Duplicate,
    /// Installed applications — trashing one uninstalls it badly
    #[value(name = "apps")]
    App,
}

#[derive(Serialize, Deserialize)]
pub struct ScanReport {
    pub scan_path: String,
    pub scan_time: String,
    pub total_bloat_bytes: u64,
    /// Entries the walk couldn't read (permission denied, vanished mid-scan)
    #[serde(default)]
    pub skipped: u64,
    pub large_files: Vec<FileEntry>,
    pub caches: Vec<FileEntry>,
    pub duplicates: Vec<DuplicateGroup>,
    pub heavy_apps: Vec<AppEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub modified: String,
    pub category: Category,
    /// Cache that may be expensive to rebuild (downloaded models, toolchains)
    #[serde(default)]
    pub review: bool,
}

#[derive(Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub hash: String,
    pub size: u64,
    /// `paths[0]` is the copy that is kept; the rest are redundant
    pub paths: Vec<PathBuf>,
    pub wasted_bytes: u64,
}

#[derive(Serialize, Deserialize)]
pub struct AppEntry {
    pub path: PathBuf,
    pub size: u64,
    pub modified: String,
    pub bundle_id: Option<String>,
}

impl ScanReport {
    /// Action targets for the chosen categories. Duplicates keep their first
    /// copy; caches are emptied rather than removed.
    pub fn targets(&self, categories: &[Category]) -> Vec<Target> {
        let mut out = Vec::new();
        for cat in categories {
            match cat {
                Category::LargeFile => out.extend(
                    self.large_files
                        .iter()
                        .map(|f| Target::Path(f.path.clone())),
                ),
                Category::Cache => out.extend(
                    self.caches
                        .iter()
                        .map(|f| Target::CacheContents(f.path.clone())),
                ),
                Category::Duplicate => out.extend(self.duplicates.iter().filter_map(|g| {
                    let (keep, redundant) = g.paths.split_first()?;
                    Some(Target::Duplicate {
                        keep: keep.clone(),
                        redundant: redundant.to_vec(),
                    })
                })),
                Category::App => {
                    out.extend(self.heavy_apps.iter().map(|a| Target::Path(a.path.clone())));
                }
            }
        }
        out
    }
}

#[derive(Clone, Copy, Default)]
pub struct ScanOptions {
    /// Also size known cache dirs and installed apps. These are system-wide
    /// and don't depend on the scan path.
    pub include_system: bool,
}

/// Live counters a frontend can poll while `build_report` runs on another thread.
#[derive(Default)]
pub struct ScanProgress {
    pub files_seen: AtomicU64,
    phase: AtomicU8,
}

impl ScanProgress {
    const WALKING: u8 = 0;
    const HASHING: u8 = 1;

    pub fn phase_label(&self) -> &'static str {
        match self.phase.load(Ordering::Relaxed) {
            Self::WALKING => "walking files",
            _ => "hashing duplicate candidates",
        }
    }
}

pub fn build_report(
    root: &Path,
    min_size: u64,
    opts: ScanOptions,
    progress: &ScanProgress,
) -> ScanReport {
    // Cache and app sizing is independent of the walk; run them side by side
    let (walk, (caches, heavy_apps)) = rayon::join(
        || walk_large_files(root, min_size, progress),
        || {
            if opts.include_system {
                rayon::join(find_caches, find_heavy_apps)
            } else {
                (Vec::new(), Vec::new())
            }
        },
    );

    progress
        .phase
        .store(ScanProgress::HASHING, Ordering::Relaxed);
    let duplicates = find_duplicates(&walk.files);
    let large_files = prune_covered(walk.files, &duplicates, &caches, &heavy_apps);

    let total_bloat: u64 = large_files.iter().map(|f| f.size).sum::<u64>()
        + caches.iter().map(|f| f.size).sum::<u64>()
        + duplicates.iter().map(|g| g.wasted_bytes).sum::<u64>()
        + heavy_apps.iter().map(|a| a.size).sum::<u64>();

    ScanReport {
        scan_path: root.display().to_string(),
        scan_time: Utc::now().to_rfc3339(),
        total_bloat_bytes: total_bloat,
        skipped: walk.skipped,
        large_files,
        caches,
        duplicates,
        heavy_apps,
    }
}

/// `build_report` with an stderr spinner — for the CLI and the pre-TUI scan.
pub fn build_report_with_spinner(root: &Path, min_size: u64, opts: ScanOptions) -> ScanReport {
    let progress = ScanProgress::default();
    let spinner = indicatif::ProgressBar::new_spinner();
    std::thread::scope(|s| {
        let job = s.spawn(|| build_report(root, min_size, opts, &progress));
        while !job.is_finished() {
            spinner.set_message(format!(
                "Scanning {} — {} files seen, {}",
                root.display(),
                progress.files_seen.load(Ordering::Relaxed),
                progress.phase_label()
            ));
            spinner.tick();
            std::thread::sleep(Duration::from_millis(100));
        }
        spinner.finish_and_clear();
        // A panic in the scan is a bug — surface it rather than return a fake report
        job.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

pub fn run_scan(root: &Path, min_size: u64, json: bool, opts: ScanOptions) -> anyhow::Result<()> {
    let report = build_report_with_spinner(root, min_size, opts);

    eprintln!("  Found {} large file(s)", report.large_files.len());
    if opts.include_system {
        eprintln!("  Found {} cache/temp location(s)", report.caches.len());
    }
    eprintln!("  Found {} duplicate group(s)", report.duplicates.len());
    if opts.include_system {
        eprintln!("  Found {} heavy app(s)", report.heavy_apps.len());
    }
    if report.skipped > 0 {
        eprintln!("  Skipped {} unreadable entr(y/ies)", report.skipped);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report_text(&report, opts);
    }

    Ok(())
}

fn rfc3339(t: SystemTime) -> String {
    DateTime::<Utc>::from(t).to_rfc3339()
}

fn mtime(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(rfc3339)
        .unwrap_or_default()
}

struct Walk {
    /// Every file >= `min_size`, biggest first
    files: Vec<FileEntry>,
    skipped: u64,
}

/// The one filesystem walk: everything >= `min_size` is both a large-file
/// candidate and a duplicate candidate.
fn walk_large_files(root: &Path, min_size: u64, progress: &ScanProgress) -> Walk {
    let mut files = Vec::new();
    let mut skipped = 0u64;
    for entry in WalkDir::new(root)
        .max_depth(SCAN_DEPTH)
        .into_iter()
        .filter_entry(|e| !should_skip(e.path()))
    {
        let Ok(entry) = entry else {
            skipped += 1;
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        progress.files_seen.fetch_add(1, Ordering::Relaxed);
        let Ok(meta) = entry.metadata() else {
            skipped += 1;
            continue;
        };
        if meta.len() >= min_size {
            files.push(FileEntry {
                path: entry.into_path(),
                size: meta.len(),
                modified: meta.modified().map(rfc3339).unwrap_or_default(),
                category: Category::LargeFile,
                review: false,
            });
        }
    }
    files.sort_by_key(|f| Reverse(f.size));
    Walk { files, skipped }
}

/// Drop large files already represented by another row, so no path is listed
/// (and actioned) twice — in particular a duplicate group's kept copy must not
/// reappear as a trashable large file.
fn prune_covered(
    large: Vec<FileEntry>,
    duplicates: &[DuplicateGroup],
    caches: &[FileEntry],
    apps: &[AppEntry],
) -> Vec<FileEntry> {
    let in_dup_group: HashSet<&Path> = duplicates
        .iter()
        .flat_map(|g| g.paths.iter().map(PathBuf::as_path))
        .collect();
    let containers: Vec<&Path> = caches
        .iter()
        .map(|c| c.path.as_path())
        .chain(apps.iter().map(|a| a.path.as_path()))
        .collect();
    large
        .into_iter()
        .filter(|f| {
            !in_dup_group.contains(f.path.as_path())
                && !containers.iter().any(|c| f.path.starts_with(c))
        })
        .collect()
}

/// Known cache/temp paths (macOS). `true` = review rather than safe.
#[cfg(not(target_os = "windows"))]
fn cache_paths() -> Vec<(PathBuf, bool)> {
    let h = |s: &str| under(&crate::home_dir(), s);
    // Not listed on purpose: ~/.docker (config + credentials), Xcode/Archives
    // (shipped builds and dSYMs), VS Code workspaceStorage (per-workspace state).
    vec![
        (h("Library/Caches"), false),
        (h("Library/Developer/Xcode/DerivedData"), false),
        (h("Library/Developer/CoreSimulator/Devices"), true),
        (h("Library/Developer/Xcode/iOS DeviceSupport"), false),
        (h("Library/Logs"), false),
        (h(".npm/_cacache"), false),
        (h(".cargo/registry"), false),
        (h(".cargo/git"), false),
        // Catch-all: holds model downloads and toolchains that are slow to refetch
        (h(".cache"), true),
        (h(".gradle/caches"), false),
        (h("Library/Application Support/Code/Cache"), false),
        (h("Library/Application Support/Code/CachedData"), false),
        (h("Library/Application Support/Slack/Cache"), false),
        (
            h("Library/Application Support/Google/Chrome/Default/Cache"),
            false,
        ),
        (
            h("Library/Application Support/Google/Chrome/Default/Code Cache"),
            false,
        ),
    ]
}

/// Known cache/temp paths (Windows). `true` = review rather than safe.
#[cfg(target_os = "windows")]
fn cache_paths() -> Vec<(PathBuf, bool)> {
    let h = |s: &str| under(&crate::home_dir(), s);
    let mut paths = vec![
        (h(".npm/_cacache"), false),
        (h(".cargo/registry"), false),
        (h(".cargo/git"), false),
        // Catch-all: holds model downloads and toolchains that are slow to refetch
        (h(".cache"), true),
        (h(".gradle/caches"), false),
        (h(".m2/repository"), false),
        (h("AppData/Local/Temp"), false),
        (h("AppData/Local/pip/cache"), false),
        (h("AppData/Local/Yarn/Cache"), false),
        (h("AppData/Local/pnpm-cache"), false),
        (h("AppData/Local/NVIDIA/DXCache"), false),
        (h("AppData/Local/NVIDIA/GLCache"), false),
        (h("AppData/Roaming/Slack/Cache"), false),
        (h("AppData/Roaming/discord/Cache"), false),
        (h("AppData/Roaming/Code/Cache"), false),
        (h("AppData/Roaming/Code/CachedData"), false),
    ];
    if let Ok(p) = std::env::var("LOCALAPPDATA").map(PathBuf::from) {
        paths.extend(
            [
                "npm-cache",
                "Microsoft/Windows/INetCache",
                "Microsoft/Windows/WebCache",
                "Microsoft/Windows/WER",
                "Google/Chrome/User Data/Default/Cache",
                "Google/Chrome/User Data/Default/Code Cache",
                "Microsoft/Edge/User Data/Default/Cache",
                "D3DSCache",
            ]
            .map(|s| (under(&p, s), false)),
        );
    }
    // System temp — needs admin rights to fully traverse, sizes what it can read
    if let Ok(p) = std::env::var("SystemRoot").map(PathBuf::from) {
        paths.push((p.join("Temp"), false));
    }
    // Same dir via two routes (e.g. LOCALAPPDATA == ~/AppData/Local) would double count
    let mut seen = HashSet::new();
    paths.retain(|(p, _)| seen.insert(p.to_string_lossy().to_lowercase()));
    paths
}

/// `base` joined with a `/`-separated relative path, using native separators.
fn under(base: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(base.to_path_buf(), |p, part| p.join(part))
}

fn find_caches() -> Vec<FileEntry> {
    let mut caches: Vec<FileEntry> = cache_paths()
        .into_par_iter()
        .filter(|(p, _)| p.is_dir())
        .map(|(path, review)| FileEntry {
            size: dir_size(&path),
            modified: mtime(&path),
            path,
            category: Category::Cache,
            review,
        })
        .collect();

    // An empty cache dir is just noise in the report
    caches.retain(|c| c.size > 0);
    caches.sort_by_key(|c| Reverse(c.size));
    caches
}

/// Total bytes of regular files under `path` (a file's own size if it is one).
pub(crate) fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .max_depth(SCAN_DEPTH)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Group `candidates` (all >= `min_size`) into sets of byte-identical files.
fn find_duplicates(candidates: &[FileEntry]) -> Vec<DuplicateGroup> {
    let mut by_size: HashMap<u64, Vec<&Path>> = HashMap::new();
    for f in candidates {
        by_size.entry(f.size).or_default().push(&f.path);
    }

    let mut groups: Vec<DuplicateGroup> = by_size
        .into_par_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .flat_map_iter(|(size, paths)| {
            // Cheap pre-filter: hash both ends; only collisions get a full read
            let by_partial = group_by(distinct_files(paths), |p| partial_hash(p, size));
            by_partial
                .into_iter()
                .flat_map(|paths| group_by_full_hash(paths, size))
                .collect::<Vec<_>>()
        })
        .collect();

    groups.sort_by_key(|g| Reverse(g.wasted_bytes));
    groups
}

fn group_by_full_hash(paths: Vec<&Path>, size: u64) -> Vec<DuplicateGroup> {
    let mut by_hash: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for p in paths {
        if let Ok(h) = hash_file(p) {
            by_hash.entry(h).or_default().push(p.to_path_buf());
        }
    }
    by_hash
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(hash, mut paths)| {
            // Deterministic kept copy: lexicographically first path
            paths.sort();
            DuplicateGroup {
                hash,
                size,
                wasted_bytes: size * (paths.len() as u64 - 1),
                paths,
            }
        })
        .collect()
}

/// Bucket paths by `key`, keeping only buckets with 2+ members. Unreadable
/// paths (key error) are dropped.
fn group_by<K: std::hash::Hash + Eq>(
    paths: Vec<&Path>,
    key: impl Fn(&Path) -> std::io::Result<K>,
) -> Vec<Vec<&Path>> {
    let mut buckets: HashMap<K, Vec<&Path>> = HashMap::new();
    for p in paths {
        if let Ok(k) = key(p) {
            buckets.entry(k).or_default().push(p);
        }
    }
    buckets.into_values().filter(|v| v.len() > 1).collect()
}

/// Collapse hard links (same inode) — "deleting" one frees nothing.
#[cfg(unix)]
fn distinct_files(paths: Vec<&Path>) -> Vec<&Path> {
    use std::os::unix::fs::MetadataExt;
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|p| {
            std::fs::metadata(p)
                .map(|m| seen.insert((m.dev(), m.ino())))
                .unwrap_or(false)
        })
        .collect()
}

/// Windows exposes file IDs only via a handle; hard links are rare there, and
/// `actions::clean` re-checks `same_file` before trashing any duplicate.
#[cfg(not(unix))]
fn distinct_files(paths: Vec<&Path>) -> Vec<&Path> {
    paths
}

/// SHA256 of the first and last `PARTIAL_CHUNK` bytes.
fn partial_hash(path: &Path, size: u64) -> std::io::Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    #[allow(clippy::cast_possible_truncation)] // PARTIAL_CHUNK is 64 KiB
    let mut buf = vec![0u8; PARTIAL_CHUNK.min(size) as usize];
    file.read_exact(&mut buf)?;
    hasher.update(&buf);
    if size > PARTIAL_CHUNK {
        file.seek(SeekFrom::Start(
            size.saturating_sub(PARTIAL_CHUNK).max(PARTIAL_CHUNK),
        ))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        hasher.update(&tail);
    }
    Ok(hasher.finalize().into())
}

/// Heavy .app bundles in /Applications and ~/Applications
#[cfg(not(target_os = "windows"))]
fn find_heavy_apps() -> Vec<AppEntry> {
    let app_dirs = [
        PathBuf::from("/Applications"),
        crate::home_dir().join("Applications"),
    ];
    let bundles: Vec<PathBuf> = app_dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("app"))
        .collect();

    sized_apps(bundles, |p| read_bundle_id(p))
}

/// Heavy installed programs: top-level dirs in Program Files / Program Files (x86)
/// and %LOCALAPPDATA%\Programs. No per-program registry lookup — dir sizes are
/// the signal here and include things the uninstall registry misses (e.g. games).
// lean-dev: bundle_id stays None on Windows — reading exe version info needs the Windows API
#[cfg(target_os = "windows")]
fn find_heavy_apps() -> Vec<AppEntry> {
    let mut app_dirs: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .map(PathBuf::from)
        .collect();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        app_dirs.push(PathBuf::from(local).join("Programs"));
    }
    // Single-file installers/exes in these roots are not "apps" for our purposes
    let programs: Vec<PathBuf> = app_dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();

    sized_apps(programs, |_| None)
}

fn sized_apps(
    dirs: Vec<PathBuf>,
    bundle_id: impl Fn(&Path) -> Option<String> + Sync,
) -> Vec<AppEntry> {
    let mut apps: Vec<AppEntry> = dirs
        .into_par_iter()
        .filter_map(|path| {
            let size = dir_size(&path);
            (size >= HEAVY_APP_BYTES).then(|| AppEntry {
                modified: mtime(&path),
                bundle_id: bundle_id(&path),
                size,
                path,
            })
        })
        .collect();
    apps.sort_by_key(|a| Reverse(a.size));
    apps
}

/// Extract `CFBundleIdentifier` from an app's Info.plist.
#[cfg(not(target_os = "windows"))]
// lean-dev: simple string search — won't parse binary plists; returns None for those
fn read_bundle_id(app_path: &Path) -> Option<String> {
    let plist_path = app_path.join("Contents/Info.plist");
    let content = std::fs::read_to_string(&plist_path).ok()?;
    let key = "<key>CFBundleIdentifier</key>";
    let idx = content.find(key)?;
    let after = &content[idx + key.len()..];
    let start = after.find("<string>")?;
    let end = after[start..].find("</string>")?;
    Some(after[start + 8..start + end].trim().to_string())
}

fn print_report_text(report: &ScanReport, opts: ScanOptions) {
    let size = crate::human_size;
    println!("\n══════════════════════════════════════════════════");
    println!("  Disk Scavenger Report");
    println!("  Scanned : {}", report.scan_path);
    println!("  Time    : {}", report.scan_time);
    println!("  Bloat   : {}", size(report.total_bloat_bytes));
    if report.skipped > 0 {
        println!("  Skipped : {} unreadable entries", report.skipped);
    }
    println!("══════════════════════════════════════════════════");

    println!("\n── Large Files (top 50) ──");
    println!(
        "   (review — your own files; offload first as backup, clean only what you can replace)"
    );
    if report.large_files.is_empty() {
        println!("  (none)");
    } else {
        for f in report.large_files.iter().take(50) {
            println!("  {:>10}  {}", size(f.size), f.path.display());
        }
        if report.large_files.len() > 50 {
            println!(
                "  ... and {} more (use --json for full list)",
                report.large_files.len() - 50
            );
        }
    }

    if opts.include_system {
        println!("\n── Caches & Temp (system-wide, not limited to the scan path) ──");
        println!("   (cleaning empties the folder; apps recreate caches on demand)");
        if report.caches.is_empty() {
            println!("  (none)");
        } else {
            for c in &report.caches {
                let tag = if c.review { "  [review]" } else { "" };
                println!("  {:>10}  {}{tag}", size(c.size), c.path.display());
            }
        }
    }

    println!("\n── Duplicates (top 20 groups) ──");
    println!("   (safe — cleaning keeps the first copy and trashes only verified identical ones)");
    if report.duplicates.is_empty() {
        println!("  (none)");
    } else {
        for g in report.duplicates.iter().take(20) {
            println!(
                "  {:>10} wasted  ({} copies, {} each)",
                size(g.wasted_bytes),
                g.paths.len(),
                size(g.size)
            );
            for (i, p) in g.paths.iter().enumerate() {
                let role = if i == 0 { "keep " } else { "extra" };
                println!("    {role}  {}", p.display());
            }
        }
        if report.duplicates.len() > 20 {
            println!("  ... and {} more groups", report.duplicates.len() - 20);
        }
    }

    if opts.include_system {
        println!("\n── Heavy Apps >500MB (system-wide) ──");
        println!(
            "   (risky — a row here IS the app; cleaning it breaks the install. Use its uninstaller)"
        );
        if report.heavy_apps.is_empty() {
            println!("  (none)");
        } else {
            for a in &report.heavy_apps {
                println!(
                    "  {:>10}  {:<30}  {}",
                    size(a.size),
                    a.bundle_id.as_deref().unwrap_or("?"),
                    a.path.display()
                );
            }
        }
    }

    println!("\n──────────────────────────────────────────────────");
    println!("  Save a report:  disk-scavenger scan --json > scan.json");
    println!(
        "  Offload:        disk-scavenger offload --dest <DIR> --json-input scan.json --category large"
    );
    println!(
        "  Clean:          disk-scavenger clean --json-input scan.json   (duplicates only by default)"
    );
    println!("                  add --category caches,duplicates,large to widen it");
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_should_skip_system_paths() {
        assert!(should_skip(Path::new("/dev/null")));
        assert!(should_skip(Path::new("/System/Library")));
        assert!(should_skip(Path::new("/usr/bin")));
        assert!(!should_skip(Path::new("/library-backup/x")));
        assert!(!should_skip(Path::new("/Users/test/Documents")));
        assert!(!should_skip(Path::new("/Applications")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_should_skip_windows_paths() {
        assert!(should_skip(Path::new(r"C:\Windows\System32")));
        assert!(should_skip(Path::new(r"c:\WINDOWS")));
        assert!(should_skip(Path::new(r"C:\$Recycle.Bin\file.bin")));
        assert!(should_skip(Path::new(r"D:\System Volume Information")));
        assert!(should_skip(Path::new(r"E:\pagefile.sys")));
        assert!(should_skip(Path::new(
            r"C:\Program Files\WindowsApps\Some.App"
        )));
        assert!(!should_skip(Path::new(r"C:\Users\me\Documents\video.mp4")));
        assert!(!should_skip(Path::new(r"C:\Program Files\MyApp")));
        // Only drive-root `Windows` is the OS dir
        assert!(!should_skip(Path::new(r"C:\Users\me\code\windows\big.iso")));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_read_bundle_id_not_found() {
        assert!(read_bundle_id(Path::new("/nonexistent/app.app")).is_none());
    }

    /// Two identical files, one different file of the same size, one unique.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![7u8; 200 * 1024];
        let mut other = big.clone();
        other[100 * 1024] = 8; // same size, same ends, different middle
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.bin"), &big).unwrap();
        std::fs::write(dir.path().join("sub/b.bin"), &big).unwrap();
        std::fs::write(dir.path().join("c.bin"), &other).unwrap();
        std::fs::write(dir.path().join("small.txt"), b"tiny").unwrap();
        dir
    }

    #[test]
    fn test_duplicates_need_full_content_match() {
        let dir = fixture();
        let walk = walk_large_files(dir.path(), 1024, &ScanProgress::default());
        assert_eq!(walk.files.len(), 3, "small.txt is under min_size");

        let groups = find_duplicates(&walk.files);
        assert_eq!(
            groups.len(),
            1,
            "c.bin shares ends with a/b but not content"
        );
        let g = &groups[0];
        assert_eq!(g.paths.len(), 2);
        assert_eq!(
            g.paths[0],
            dir.path().join("a.bin"),
            "kept copy is sorted first"
        );
        assert_eq!(g.wasted_bytes, 200 * 1024);
    }

    #[test]
    fn test_no_path_listed_in_two_categories() {
        let dir = fixture();
        let report = build_report(
            dir.path(),
            1024,
            ScanOptions::default(),
            &ScanProgress::default(),
        );
        let dup_paths: HashSet<&PathBuf> =
            report.duplicates.iter().flat_map(|g| &g.paths).collect();
        assert!(!dup_paths.is_empty());
        for f in &report.large_files {
            assert!(
                !dup_paths.contains(&f.path),
                "{} listed twice",
                f.path.display()
            );
        }
        // c.bin isn't a duplicate, so it stays a large file
        assert_eq!(report.large_files.len(), 1);
        // Bloat counts each byte once: 200K wasted + c.bin
        assert_eq!(report.total_bloat_bytes, 2 * 200 * 1024);
    }

    #[test]
    fn test_large_files_inside_caches_are_pruned() {
        let cache = FileEntry {
            path: PathBuf::from("/home/u/.cache"),
            size: 10,
            modified: String::new(),
            category: Category::Cache,
            review: true,
        };
        let inside = FileEntry {
            path: PathBuf::from("/home/u/.cache/model.bin"),
            category: Category::LargeFile,
            ..cache.clone()
        };
        let outside = FileEntry {
            path: PathBuf::from("/home/u/movie.mkv"),
            ..inside.clone()
        };
        let kept = prune_covered(vec![inside, outside], &[], &[cache], &[]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, PathBuf::from("/home/u/movie.mkv"));
    }

    #[test]
    fn test_targets_filter_by_category() {
        let dir = fixture();
        let report = build_report(
            dir.path(),
            1024,
            ScanOptions::default(),
            &ScanProgress::default(),
        );
        let dups = report.targets(&[Category::Duplicate]);
        assert_eq!(dups.len(), 1);
        assert!(matches!(&dups[0], Target::Duplicate { redundant, .. } if redundant.len() == 1));
        assert!(report.targets(&[Category::App]).is_empty());
        assert_eq!(
            report
                .targets(&[Category::Duplicate, Category::LargeFile])
                .len(),
            2
        );
    }

    #[test]
    fn test_old_report_json_still_parses() {
        // Reports written before `skipped`/`review` and the Category enum existed
        let json = r#"{"scan_path":"/","scan_time":"","total_bloat_bytes":0,
            "large_files":[{"path":"/a","size":1,"modified":"","category":"large_file"}],
            "caches":[{"path":"/c","size":1,"modified":"","category":"cache"}],
            "duplicates":[],"heavy_apps":[]}"#;
        let r: ScanReport = serde_json::from_str(json).unwrap();
        assert_eq!(r.skipped, 0);
        assert_eq!(r.caches[0].category, Category::Cache);
        assert!(!r.caches[0].review);
    }
}
