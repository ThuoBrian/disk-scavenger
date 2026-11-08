use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use disk_scavenger::actions::{self, Target};
use disk_scavenger::scanner::{self, Category, ScanOptions};
use disk_scavenger::{home_dir, human_size, parse_size_arg, transfer, tui};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "disk-scavenger",
    version,
    about = "Scan disk for bloat, offload to external drive, or clean up"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Args)]
struct ScanArgs {
    /// Path to scan (defaults to home directory)
    #[arg(long)]
    path: Option<PathBuf>,

    /// Minimum file size to flag (e.g. 500MB, 1.5GB)
    #[arg(long, value_parser = parse_size_arg, default_value = "500MB")]
    min_size: u64,

    /// Skip system-wide caches and installed apps (they ignore --path)
    #[arg(long)]
    no_system: bool,
}

impl ScanArgs {
    fn root(&self) -> PathBuf {
        self.path.clone().unwrap_or_else(home_dir)
    }

    fn options(&self) -> ScanOptions {
        ScanOptions {
            include_system: !self.no_system,
        }
    }
}

#[derive(Args)]
struct TargetArgs {
    /// Files or folders to act on
    files: Vec<PathBuf>,

    /// Read targets from a JSON scan report
    #[arg(long)]
    json_input: Option<PathBuf>,

    /// Which report categories to act on (with --json-input). `apps` must be
    /// named explicitly — trashing an app breaks its install.
    #[arg(long, value_enum, value_delimiter = ',', default_value = "duplicates")]
    category: Vec<Category>,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan disk for large files, caches, duplicates, and heavy apps
    Scan {
        #[command(flatten)]
        scan: ScanArgs,

        /// Output as JSON (for piping into offload/clean)
        #[arg(long)]
        json: bool,
    },
    /// Copy files to external drive with SHA256 verification
    Offload {
        /// Destination directory (e.g. /Volumes/MyDrive/offloaded or E:\offloaded)
        #[arg(long)]
        dest: PathBuf,

        #[command(flatten)]
        input: TargetArgs,

        /// Trash source files after every copy is verified
        #[arg(long)]
        delete_after: bool,

        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Move files to trash (safe delete)
    Clean {
        #[command(flatten)]
        input: TargetArgs,

        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Interactive TUI — scan, review, select, and act
    Interactive {
        #[command(flatten)]
        scan: ScanArgs,

        /// Default offload destination (prompted if not set)
        #[arg(long)]
        dest: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Scan { scan, json } => {
            scanner::run_scan(&scan.root(), scan.min_size, json, scan.options())
        }
        Commands::Offload {
            dest,
            input,
            delete_after,
            yes,
        } => offload(&resolve_targets(input)?, &dest, delete_after, yes),
        Commands::Clean { input, yes } => clean(&resolve_targets(input)?, yes),
        Commands::Interactive { scan, dest } => {
            let report =
                scanner::build_report_with_spinner(&scan.root(), scan.min_size, scan.options());
            match tui::run_tui(&report)? {
                tui::TuiAction::Offload(targets) => {
                    let dest = match dest {
                        Some(d) => d,
                        None => prompt_dest()?,
                    };
                    offload(&targets, &dest, false, false)
                }
                tui::TuiAction::Clean(targets) => clean(&targets, false),
                tui::TuiAction::Quit => {
                    eprintln!("Exited without action.");
                    Ok(())
                }
            }
        }
    }
}

fn resolve_targets(input: TargetArgs) -> Result<Vec<Target>> {
    let mut targets: Vec<Target> = input.files.into_iter().map(Target::Path).collect();
    if let Some(path) = input.json_input {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("Cannot read JSON input: {}", path.display()))?;
        let report: scanner::ScanReport = serde_json::from_str(&data)
            .with_context(|| format!("Not a scan report: {}", path.display()))?;
        let from_report = report.targets(&input.category);
        if from_report.is_empty() {
            eprintln!(
                "Note: the report has nothing in categories {:?}.",
                input.category
            );
        }
        targets.extend(from_report);
    }
    if targets.is_empty() {
        anyhow::bail!(
            "Nothing to do. Pass paths as arguments, or --json-input with a scan report \
             (and --category to pick large, caches, duplicates, apps)."
        );
    }
    Ok(targets)
}

/// y/N prompt on stderr; anything but "y" cancels.
fn confirm(question: &str) -> Result<bool> {
    eprint!("\n{question} [y/N] ");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}

fn clean(targets: &[Target], yes: bool) -> Result<()> {
    let total: u64 = targets.iter().map(Target::size).sum();
    eprintln!(
        "Moving {} item(s) to the {} ({}):",
        targets.len(),
        actions::TRASH_NAME,
        human_size(total)
    );
    for line in targets.iter().flat_map(Target::describe) {
        eprintln!("  {line}");
    }
    for note in actions::trash_notes(total) {
        eprintln!("Note: {note}");
    }

    if !yes && !confirm("Proceed?")? {
        eprintln!("Cancelled.");
        return Ok(());
    }

    let summary = actions::clean(targets);
    for (path, why) in &summary.problems {
        eprintln!("  skipped {}: {why}", path.display());
    }
    eprintln!("Done. {}", summary.report());
    Ok(())
}

fn offload(
    targets: &[Target],
    dest: &std::path::Path,
    delete_after: bool,
    yes: bool,
) -> Result<()> {
    let (files, skipped) = actions::offload_files(targets);
    for (path, why) in &skipped {
        eprintln!("  skipping {}: {why}", path.display());
    }
    if files.is_empty() {
        anyhow::bail!("No regular files to offload.");
    }
    if !yes {
        let total: u64 = files
            .iter()
            .filter_map(|f| f.metadata().ok())
            .map(|m| m.len())
            .sum();
        let verb = if delete_after { "Move" } else { "Copy" };
        if !confirm(&format!(
            "{verb} {} file(s) ({}) to {}?",
            files.len(),
            human_size(total),
            dest.display()
        ))? {
            eprintln!("Cancelled.");
            return Ok(());
        }
    }
    let summary = transfer::offload(&files, dest, delete_after)?;
    if summary.failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} file(s) failed to offload", summary.failed.len())
    }
}

fn prompt_dest() -> Result<PathBuf> {
    eprint!("Offload destination folder: ");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let path = PathBuf::from(input.trim());
    if path.as_os_str().is_empty() {
        anyhow::bail!("No destination provided.");
    }
    Ok(path)
}
