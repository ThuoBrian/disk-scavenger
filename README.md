<p align="center">
  <img src="assets/logo.svg" alt="disk-scavenger logo" width="128">
</p>

<h1 align="center">disk-scavenger</h1>

<p align="center">Find what's filling your disk, back it up safely, and clear out the rest.</p>

---

disk-scavenger scans a folder for large files, duplicate files, cache folders and big installed apps. You pick what to deal with, then either copy it to another drive (every copy is checked with SHA256) or move it to the Trash. It never deletes files directly.

It runs on Windows and macOS, and comes as a desktop app, a terminal UI and a command-line tool.

## What it finds

| Category | What it is | Is it safe to remove? |
| --- | --- | --- |
| Large files | Files over the size limit (500 MB by default) | **Review.** They're your files, so back them up first. |
| Duplicates | Files with identical content | **Safe.** One copy is always kept. |
| Caches & temp | Known cache folders (browsers, npm, pip, cargo, Temp…) | **Safe.** The folder is emptied and apps rebuild it. |
| Installed apps | Programs over 500 MB | **Risky.** Use the app's uninstaller instead. |

Caches and apps are system-wide, so they show up whichever folder you scan.

## Install

You need [Rust](https://rustup.rs) 1.95 or newer.

```bash
cargo build --release --features gui
```

That builds both programs into `target/release/`:

- `disk-scavenger-app` is the desktop app (about 6 MB)
- `disk-scavenger` is the terminal UI and command-line tool (about 1.7 MB)

Leave out `--features gui` if you only want the command-line tool.

## Desktop app

Open `disk-scavenger-app`, choose a folder and a size limit, and press **Scan**. On Windows you can also drag a folder onto `disk-scavenger-app.exe` to scan it straight away.

Results are grouped by category on the left. Click a row to see exactly which files it covers, tick the ones you want, then choose **Offload** to copy them to another folder or **Move to Recycle Bin** (**Move to Trash** on macOS). The app follows your system's light or dark theme.

## Terminal UI

```bash
disk-scavenger interactive
disk-scavenger interactive --path ~/Downloads --dest E:\backup
```

Move with the arrow keys, tick rows with `Space`, then press `O` to offload or `C` to clean. `A` selects every safe row, `N` clears the selection, and `Q` quits. The panel at the bottom shows what the highlighted row will touch.

## Command line

Scan and print a report:

```bash
disk-scavenger scan                                  # your home folder, 500 MB limit
disk-scavenger scan --path ~/Downloads --min-size 1.5GB
disk-scavenger scan --no-system                      # skip caches and apps
disk-scavenger scan --json > scan.json               # save the report for later
```

Copy files to another drive:

```bash
disk-scavenger offload --dest E:\backup ~/Videos/holiday.mp4
disk-scavenger offload --dest E:\backup --json-input scan.json --category large
```

Add `--delete-after` to move the originals to the Trash once every copy has been checked.

Move things to the Trash:

```bash
disk-scavenger clean ~/Downloads/old-installer.exe
disk-scavenger clean --json-input scan.json                       # duplicates only
disk-scavenger clean --json-input scan.json --category duplicates,caches
```

When reading a saved report, `offload` and `clean` only touch duplicates unless you name other categories with `--category` (`large`, `caches`, `duplicates`, `apps`). Both commands show what they're about to do and ask before going ahead. Pass `--yes` to skip the question.

## How it keeps your files safe

- Copies never overwrite anything at the destination, and each one is checked against the original before it counts.
- Folder structure is kept when copying, so two files with the same name don't collide.
- Before removing a duplicate, it checks again that the kept copy still exists and still matches. If not, it leaves that group alone.
- "Select all" only picks rows marked safe. Your own files and installed apps have to be ticked one at a time.
- If one file can't be moved (say, it's in use), the rest still go.

Disk space comes back only when you empty the Trash or Recycle Bin. On Windows, a file too big for the Recycle Bin can be deleted for good without a warning, so offload anything you might want back first.

## Platform notes

| | Windows | macOS |
| --- | --- | --- |
| Cache folders | `%LOCALAPPDATA%\Temp`, npm, pip, Yarn, pnpm, Chrome, Edge, Windows error reports, shader caches | `~/Library/Caches`, Xcode DerivedData, npm, cargo, Gradle, VS Code, Slack, Chrome |
| Installed apps | Folders in Program Files and `%LOCALAPPDATA%\Programs` | `.app` bundles in `/Applications` and `~/Applications` |
| Skipped while scanning | `Windows`, `$Recycle.Bin`, `System Volume Information`, page and hibernation files | `/System`, `/usr`, `/private/var/vm` |

A few folders are left off the cache list on purpose: `~/.docker` (it holds your settings and logins), Xcode Archives (your built apps) and VS Code's `workspaceStorage` (per-project state).

## Development

```bash
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

## License

MIT
