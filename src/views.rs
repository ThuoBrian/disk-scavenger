//! Shared row model for the two frontends (TUI + GUI): flattens a `ScanReport`
//! into a scrollable list of category headers and selectable items.
use std::path::{Path, PathBuf};

use crate::actions::Target;
use crate::human_size;
use crate::scanner::{Category, ScanReport};

/// Per-item harm assessment shown before a delete is allowed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Safety {
    /// Removing it causes no lasting harm (apps rebuild, or a verified
    /// identical copy stays on disk).
    Safe(&'static str),
    /// Personal file, or a cache that's costly to rebuild — the tool cannot
    /// claim it's harmless. Trash is the undo path; offloading first is safest.
    Review(&'static str),
    /// Removing it removes the thing it belongs to (e.g. the app itself).
    Risky(&'static str),
}

impl Safety {
    pub fn tag(&self) -> &'static str {
        match self {
            Safety::Safe(_) => "safe",
            Safety::Review(_) => "review",
            Safety::Risky(_) => "risky",
        }
    }

    pub fn reason(&self) -> &'static str {
        match self {
            Safety::Safe(r) | Safety::Review(r) | Safety::Risky(r) => r,
        }
    }
}

#[derive(Clone, Default)]
pub struct DisplayItem {
    /// One-line text for the TUI (size + path)
    pub label: String,
    pub is_header: bool,
    /// Which section the row (or header) belongs to
    pub category: Option<Category>,
    /// Short name for the GUI's first line: file, folder or app name
    pub name: String,
    /// Dimmed second line for the GUI: where it lives, copy counts, bundle id
    pub location: String,
    /// `None` for category headers
    pub target: Option<Target>,
    pub size: u64,
    pub selected: bool,
    /// `None` for category headers
    pub safety: Option<Safety>,
}

impl DisplayItem {
    fn header(category: Category, label: String) -> Self {
        Self {
            label,
            is_header: true,
            category: Some(category),
            ..Self::default()
        }
    }

    /// Lines for a detail view: the safety reason, then every path affected.
    pub fn details(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(s) = self.safety {
            lines.push(format!("[{}] {}", s.tag(), s.reason()));
        }
        if let Some(t) = &self.target {
            lines.extend(t.describe());
        }
        lines
    }
}

/// Builder for a selectable row; keeps `build_items` readable.
struct Row {
    category: Category,
    label: String,
    name: String,
    location: String,
}

impl Row {
    fn new(category: Category, path: &Path) -> Self {
        Self {
            category,
            label: String::new(),
            name: file_name(path),
            location: path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        }
    }

    fn finish(self, target: Target, size: u64, safety: Safety) -> DisplayItem {
        DisplayItem {
            label: self.label,
            is_header: false,
            category: Some(self.category),
            name: self.name,
            location: self.location,
            target: Some(target),
            size,
            selected: false,
            safety: Some(safety),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

// Per-category harm verdicts: what deleting this row does to the system.
const REVIEW_FILE: Safety =
    Safety::Review("your own file — offload first, or rely on Trash as undo");
const SAFE_CACHE: Safety = Safety::Safe("folder is emptied; apps recreate caches on demand");
const REVIEW_CACHE: Safety =
    Safety::Review("may hold model downloads or toolchains that are slow to fetch again");
const SAFE_DUPLICATE: Safety = Safety::Safe("re-verified identical copy stays on disk");
const RISKY_APP: Safety = Safety::Risky("this is the installed app itself — uninstall instead");

/// Display name and the explanation shown above a category's rows.
/// The `Safety` is the category's usual verdict (single rows may differ).
pub fn category_info(category: Category) -> (&'static str, Safety) {
    match category {
        Category::LargeFile => (
            "Large files",
            Safety::Review(
                "Your own files. Offload them to a backup drive first; cleaning only moves them \
                 to the trash, which is your undo.",
            ),
        ),
        Category::Cache => (
            "Caches & temp",
            Safety::Safe(
                "System-wide cache and temp folders. Cleaning empties each folder and keeps the \
                 folder itself; apps rebuild what they need.",
            ),
        ),
        Category::Duplicate => (
            "Duplicates",
            Safety::Safe(
                "Identical copies. The first copy is always kept, and every extra copy is \
                 re-checked against it before it is removed.",
            ),
        ),
        Category::App => (
            "Installed apps",
            Safety::Risky(
                "Each row is an installed program. Removing it breaks the app, so use its \
                 uninstaller instead. Listed so you can see where the space goes.",
            ),
        ),
    }
}

/// Flatten the `ScanReport` into a single list with category headers.
/// Empty categories (e.g. system-wide ones that weren't scanned) are omitted.
pub fn build_items(report: &ScanReport) -> Vec<DisplayItem> {
    let mut items = Vec::new();

    items.push(DisplayItem::header(
        Category::LargeFile,
        format!(" Large Files ({})", report.large_files.len()),
    ));
    for f in &report.large_files {
        let mut row = Row::new(Category::LargeFile, &f.path);
        row.label = format!("{}  {}", human_size(f.size), f.path.display());
        items.push(row.finish(Target::Path(f.path.clone()), f.size, REVIEW_FILE));
    }

    if !report.caches.is_empty() {
        items.push(DisplayItem::header(
            Category::Cache,
            format!(" Caches & Temp — system-wide ({})", report.caches.len()),
        ));
        for f in &report.caches {
            let mut row = Row::new(Category::Cache, &f.path);
            row.label = format!("{}  {}", human_size(f.size), f.path.display());
            let safety = if f.review { REVIEW_CACHE } else { SAFE_CACHE };
            items.push(row.finish(Target::CacheContents(f.path.clone()), f.size, safety));
        }
    }

    // Each group is one row; acting on it touches all but the first copy.
    items.push(DisplayItem::header(
        Category::Duplicate,
        format!(" Duplicates ({} groups)", report.duplicates.len()),
    ));
    for g in &report.duplicates {
        let Some((keep, redundant)) = g.paths.split_first() else {
            continue;
        };
        let mut row = Row::new(Category::Duplicate, keep);
        row.label = format!(
            "{} wasted  {}  ({} copies, {} each)",
            human_size(g.wasted_bytes),
            row.name,
            g.paths.len(),
            human_size(g.size)
        );
        row.location = format!(
            "{} copies · {} each · kept in {}",
            g.paths.len(),
            human_size(g.size),
            row.location
        );
        let target = Target::Duplicate {
            keep: keep.clone(),
            redundant: redundant.to_vec(),
        };
        items.push(row.finish(target, g.wasted_bytes, SAFE_DUPLICATE));
    }

    if !report.heavy_apps.is_empty() {
        items.push(DisplayItem::header(
            Category::App,
            format!(" Heavy Apps — system-wide ({})", report.heavy_apps.len()),
        ));
        for a in &report.heavy_apps {
            let mut row = Row::new(Category::App, &a.path);
            row.label = format!(
                "{}  {}  {}",
                human_size(a.size),
                a.bundle_id.as_deref().unwrap_or("?"),
                a.path.display()
            );
            if let Some(id) = &a.bundle_id {
                row.location = format!("{id} · {}", row.location);
            }
            items.push(row.finish(Target::Path(a.path.clone()), a.size, RISKY_APP));
        }
    }

    items
}

/// Row count and total size per category, in display order, skipping empty ones.
pub fn category_totals(items: &[DisplayItem]) -> Vec<(Category, usize, u64)> {
    let mut out: Vec<(Category, usize, u64)> = Vec::new();
    for item in items.iter().filter(|i| !i.is_header) {
        let Some(cat) = item.category else { continue };
        match out.iter_mut().find(|(c, _, _)| *c == cat) {
            Some(entry) => {
                entry.1 += 1;
                entry.2 += item.size;
            }
            None => out.push((cat, 1, item.size)),
        }
    }
    out
}

pub fn selected_targets(items: &[DisplayItem]) -> Vec<Target> {
    items
        .iter()
        .filter(|i| i.selected)
        .filter_map(|i| i.target.clone())
        .collect()
}

/// "Select all" only ever picks rows marked safe; review and risky rows
/// must be ticked one at a time.
pub fn select_all_safe(items: &mut [DisplayItem]) {
    for item in items {
        item.selected = matches!(item.safety, Some(Safety::Safe(_)));
    }
}

/// True if any reported problem path belongs to this row.
fn has_problem(item: &DisplayItem, problems: &[(PathBuf, String)]) -> bool {
    item.target.as_ref().is_some_and(|t| {
        t.roots()
            .iter()
            .any(|root| problems.iter().any(|(p, _)| p.starts_with(root)))
    })
}

/// After a clean: drop rows that were fully handled; rows with a problem stay
/// selected so the user can see what's left and retry.
pub fn apply_clean(items: &mut Vec<DisplayItem>, problems: &[(PathBuf, String)]) {
    items.retain(|i| !i.selected || has_problem(i, problems));
}

/// After an offload: mark rows whose files all landed, and deselect them so
/// the next action (usually Clean) starts from a clear selection.
pub fn apply_offload(items: &mut [DisplayItem], failed: &[(PathBuf, String)]) {
    for item in items.iter_mut().filter(|i| i.selected) {
        if !has_problem(item, failed) {
            item.selected = false;
            if !item.label.starts_with(OFFLOADED) {
                item.label = format!("{OFFLOADED}{}", item.label);
            }
        }
    }
}

const OFFLOADED: &str = "[offloaded] ";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{AppEntry, DuplicateGroup, FileEntry};

    fn report() -> ScanReport {
        let entry = |p: &str, category| FileEntry {
            path: PathBuf::from(p),
            size: 1000,
            modified: String::new(),
            category,
            review: false,
        };
        ScanReport {
            scan_path: "/test".to_string(),
            scan_time: String::new(),
            total_bloat_bytes: 0,
            skipped: 0,
            large_files: vec![entry("/test/video.mp4", Category::LargeFile)],
            caches: vec![entry("/test/Library/Caches", Category::Cache)],
            duplicates: vec![DuplicateGroup {
                hash: "abc".to_string(),
                size: 1000,
                paths: vec![PathBuf::from("/test/a"), PathBuf::from("/test/b")],
                wasted_bytes: 1000,
            }],
            heavy_apps: vec![AppEntry {
                path: PathBuf::from("/Applications/App.app"),
                size: 1000,
                modified: String::new(),
                bundle_id: None,
            }],
        }
    }

    fn rows(items: &[DisplayItem]) -> Vec<&DisplayItem> {
        items.iter().filter(|i| !i.is_header).collect()
    }

    #[test]
    fn test_safety_verdicts() {
        let items = build_items(&report());
        let verdicts: Vec<Safety> = rows(&items)
            .iter()
            .map(|i| i.safety.expect("non-header rows carry a verdict"))
            .collect();
        assert_eq!(verdicts.len(), 4);
        assert!(matches!(verdicts[0], Safety::Review(_))); // large file
        assert!(matches!(verdicts[1], Safety::Safe(_))); // cache
        assert!(matches!(verdicts[2], Safety::Safe(_))); // duplicate (keeps first copy)
        assert!(matches!(verdicts[3], Safety::Risky(_))); // app bundle
    }

    #[test]
    fn test_duplicate_row_keeps_first_copy() {
        let items = build_items(&report());
        let dup = rows(&items)[2];
        assert_eq!(
            dup.target,
            Some(Target::Duplicate {
                keep: PathBuf::from("/test/a"),
                redundant: vec![PathBuf::from("/test/b")],
            })
        );
        // The detail view names both the kept and the removed copy
        let details = dup.details().join("\n");
        assert!(details.contains("keep") && details.contains("remove"));
    }

    #[test]
    fn test_select_all_never_picks_risky_or_review() {
        let mut items = build_items(&report());
        select_all_safe(&mut items);
        for i in rows(&items) {
            assert_eq!(
                i.selected,
                matches!(i.safety, Some(Safety::Safe(_))),
                "{}",
                i.label
            );
        }
        assert_eq!(selected_targets(&items).len(), 2); // cache + duplicate
    }

    #[test]
    fn test_apply_clean_keeps_rows_with_problems() {
        let mut items = build_items(&report());
        select_all_safe(&mut items);
        let problems = vec![(PathBuf::from("/test/b"), "changed".to_string())];
        apply_clean(&mut items, &problems);
        let left: Vec<_> = rows(&items).iter().map(|i| i.label.clone()).collect();
        assert_eq!(
            left.len(),
            3,
            "cache row removed, duplicate row kept: {left:?}"
        );
        assert!(
            rows(&items)
                .iter()
                .any(|i| i.selected && matches!(i.target, Some(Target::Duplicate { .. })))
        );
    }

    #[test]
    fn test_rows_carry_category_name_and_location() {
        let items = build_items(&report());
        let video = rows(&items)[0];
        assert_eq!(video.category, Some(Category::LargeFile));
        assert_eq!(video.name, "video.mp4");
        assert_eq!(video.location, PathBuf::from("/test").display().to_string());
        let dup = rows(&items)[2];
        assert_eq!(dup.name, "a");
        assert!(dup.location.starts_with("2 copies"), "{}", dup.location);
    }

    #[test]
    fn test_category_totals() {
        let items = build_items(&report());
        assert_eq!(
            category_totals(&items),
            vec![
                (Category::LargeFile, 1, 1000),
                (Category::Cache, 1, 1000),
                (Category::Duplicate, 1, 1000),
                (Category::App, 1, 1000),
            ]
        );
    }
}
