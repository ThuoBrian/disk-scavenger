//! GUI frontend — `disk-scavenger-app` binary, built only with `--features gui`.
//!
//! Three screens: pick what to scan → scanning (background thread, so the
//! window stays responsive) → results, where rows are reviewed by category and
//! offloaded or moved to the trash. Layout lives here and in `results.rs`;
//! colors, fonts and styled widgets live in `theme.rs`.
#![cfg(feature = "gui")]
// Build as a Windows GUI program so no console window opens alongside the app.
// Debug builds keep the console for log output. Without a console, stderr
// writes (e.g. offload progress) are silently discarded, which is fine here.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod results;
mod theme;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use disk_scavenger::actions::{self, CleanSummary};
use disk_scavenger::scanner::{self, Category, ScanOptions, ScanProgress};
use disk_scavenger::transfer::{self, OffloadSummary};
use disk_scavenger::views::{self, DisplayItem, build_items, selected_targets};
use disk_scavenger::{home_dir, parse_size_arg};
use eframe::egui::{self, Align, CentralPanel, Layout, RichText, TextEdit, Ui};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("disk-scavenger")
            .with_inner_size([1100.0, 660.0])
            .with_min_inner_size([860.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "disk-scavenger",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            let mut app = ScavengerApp::new();
            // A folder passed on the command line (or dropped onto the .exe in
            // Explorer, which passes it the same way) is scanned straight away
            if let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from)
                && dir.is_dir()
            {
                app.path = dir.display().to_string();
                app.start_scan();
            }
            Ok(Box::new(app))
        }),
    )
}

/// Messages from the background scan/action thread to the UI thread.
enum BgMsg {
    Scanned(scanner::ScanReport),
    Cleaned(CleanSummary),
    Offloaded(Result<OffloadSummary, String>, usize),
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Start,
    Scanning,
    Results,
}

#[derive(Clone, Copy)]
enum BannerKind {
    Success,
    Warning,
    Error,
}

/// Threshold choices on the start screen; the last one means "custom".
const SIZE_PRESETS: [(&str, u64); 5] = [
    ("100 MB", 100 << 20),
    ("500 MB", 500 << 20),
    ("1 GB", 1 << 30),
    ("5 GB", 5 << 30),
    ("Custom…", 0),
];
const CUSTOM_PRESET: usize = SIZE_PRESETS.len() - 1;

struct ScavengerApp {
    screen: Screen,
    // --- start screen ---
    path: String,
    size_preset: usize,
    custom_size: String,
    include_system: bool,
    path_error: Option<String>,
    size_error: Option<String>,
    // --- background work ---
    rx: Option<Receiver<BgMsg>>,
    progress: Arc<ScanProgress>,
    scan_label: String,
    /// Set while an offload/clean runs: results stay visible but locked
    acting: Option<String>,
    // --- results ---
    items: Vec<DisplayItem>,
    skipped: u64,
    active: Option<Category>,
    /// Index into `items` of the row shown in the detail panel
    focused: Option<usize>,
    dest: String,
    banner: Option<(BannerKind, String)>,
    show_clean_modal: bool,
}

impl ScavengerApp {
    fn new() -> Self {
        Self {
            screen: Screen::Start,
            path: home_dir().display().to_string(),
            size_preset: 1,
            custom_size: "250MB".to_string(),
            include_system: true,
            path_error: None,
            size_error: None,
            rx: None,
            progress: Arc::default(),
            scan_label: String::new(),
            acting: None,
            items: Vec::new(),
            skipped: 0,
            active: None,
            focused: None,
            dest: String::new(),
            banner: None,
            show_clean_modal: false,
        }
    }

    fn min_size(&self) -> Result<u64, String> {
        if self.size_preset == CUSTOM_PRESET {
            parse_size_arg(self.custom_size.trim())
        } else {
            Ok(SIZE_PRESETS[self.size_preset].1)
        }
    }

    fn start_scan(&mut self) {
        let path = PathBuf::from(self.path.trim());
        self.path_error = (!path.is_dir()).then(|| "This folder doesn't exist.".to_string());
        let min_size = self.min_size();
        self.size_error = min_size.as_ref().err().cloned();
        let (None, None, Ok(min_size)) = (&self.path_error, &self.size_error, min_size) else {
            return;
        };

        let opts = ScanOptions {
            include_system: self.include_system,
        };
        self.banner = None;
        self.scan_label = path.display().to_string();
        self.screen = Screen::Scanning;
        self.progress = Arc::default();
        let progress = Arc::clone(&self.progress);
        self.rx = Some(spawn(move |tx| {
            let report = scanner::build_report(&path, min_size, opts, &progress);
            let _ = tx.send(BgMsg::Scanned(report));
        }));
    }

    fn start_clean(&mut self) {
        let targets = selected_targets(&self.items);
        if targets.is_empty() {
            return;
        }
        self.begin_action(format!(
            "Moving {} item(s) to the {} …",
            targets.len(),
            actions::TRASH_NAME
        ));
        self.rx = Some(spawn(move |tx| {
            let _ = tx.send(BgMsg::Cleaned(actions::clean(&targets)));
        }));
    }

    fn start_offload(&mut self) {
        let dest = PathBuf::from(self.dest.trim());
        if self.dest.trim().is_empty() || !dest.is_dir() {
            self.banner = Some((
                BannerKind::Error,
                "Choose an existing destination folder first (Browse… next to Offload).".into(),
            ));
            return;
        }
        let (files, skipped) = actions::offload_files(&selected_targets(&self.items));
        if files.is_empty() {
            self.banner = Some((
                BannerKind::Warning,
                "Nothing to offload: cache and app folders can't be offloaded, only files.".into(),
            ));
            return;
        }
        self.begin_action(format!(
            "Copying {} file(s) to {} …",
            files.len(),
            dest.display()
        ));
        self.rx = Some(spawn(move |tx| {
            let result = transfer::offload(&files, &dest, false).map_err(|e| format!("{e:#}"));
            let _ = tx.send(BgMsg::Offloaded(result, skipped.len()));
        }));
    }

    fn begin_action(&mut self, label: String) {
        self.banner = None;
        self.show_clean_modal = false;
        self.acting = Some(label);
    }

    fn finish(&mut self, msg: BgMsg) {
        self.acting = None;
        match msg {
            BgMsg::Scanned(report) => {
                self.skipped = report.skipped;
                self.items = build_items(&report);
                self.active = None;
                self.focused = None;
                self.screen = Screen::Results;
            }
            BgMsg::Cleaned(summary) => {
                views::apply_clean(&mut self.items, &summary.problems);
                self.focused = None; // indices shifted
                let mut msg = summary.report();
                for note in actions::trash_notes(summary.bytes) {
                    msg.push(' ');
                    msg.push_str(&note);
                }
                self.banner = Some(if let Some((path, why)) = summary.problems.first() {
                    let _ = write!(
                        msg,
                        " First problem: {}: {why}. Rows still ticked weren't fully cleaned.",
                        path.display()
                    );
                    (BannerKind::Warning, msg)
                } else {
                    (BannerKind::Success, msg)
                });
            }
            BgMsg::Offloaded(Ok(summary), skipped) => {
                views::apply_offload(&mut self.items, &summary.failed);
                let mut msg = format!("{} file(s) copied and verified.", summary.copied);
                if skipped > 0 {
                    let _ = write!(
                        msg,
                        " {skipped} folder(s) skipped (only files are offloaded)."
                    );
                }
                self.banner = Some(if summary.failed.is_empty() {
                    (BannerKind::Success, msg)
                } else {
                    let _ = write!(
                        msg,
                        " {} failed; those rows are still ticked.",
                        summary.failed.len()
                    );
                    (BannerKind::Warning, msg)
                });
            }
            BgMsg::Offloaded(Err(e), _) => self.banner = Some((BannerKind::Error, e)),
        }
    }

    // ---------------------------------------------------------------- screens

    fn start_screen(&mut self, ui: &mut Ui) {
        CentralPanel::default().show(ui, |ui| {
            centered_card(ui, 540.0, |ui| {
                if let Some((kind, text)) = &self.banner
                    && results::banner(ui, *kind, text)
                {
                    self.banner = None;
                }
                ui.label(RichText::new("disk-scavenger").heading().size(28.0));
                ui.label(
                    RichText::new(
                        "Find large files, duplicates and caches, then copy them to a \
                         backup drive or move them to the trash.",
                    )
                    .color(theme::palette(ui).muted),
                );
                ui.add_space(18.0);

                field_label(ui, "Folder to scan");
                ui.horizontal(|ui| {
                    let edit = TextEdit::singleline(&mut self.path)
                        .desired_width(ui.available_width() - 90.0);
                    if ui.add(edit).changed() {
                        self.path_error = None;
                    }
                    if theme::secondary_button(ui, "Browse…", true).clicked()
                        && let Some(p) = pick_folder(&self.path)
                    {
                        self.path = p.display().to_string();
                        self.path_error = None;
                    }
                });
                ui.horizontal(|ui| {
                    for (name, path) in quick_folders() {
                        let current = Path::new(self.path.trim()) == path;
                        if ui.selectable_label(current, name).clicked() {
                            self.path = path.display().to_string();
                            self.path_error = None;
                        }
                    }
                });
                field_error(ui, self.path_error.as_deref());
                ui.add_space(12.0);

                field_label(ui, "Flag files larger than");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("size-preset")
                        .width(130.0)
                        .selected_text(SIZE_PRESETS[self.size_preset].0)
                        .show_ui(ui, |ui| {
                            for (i, (name, _)) in SIZE_PRESETS.iter().enumerate() {
                                if ui
                                    .selectable_value(&mut self.size_preset, i, *name)
                                    .changed()
                                {
                                    self.size_error = None;
                                }
                            }
                        });
                    if self.size_preset == CUSTOM_PRESET
                        && ui
                            .add(
                                TextEdit::singleline(&mut self.custom_size)
                                    .hint_text("e.g. 750MB or 1.5GB")
                                    .desired_width(150.0),
                            )
                            .changed()
                    {
                        self.size_error = None;
                    }
                });
                field_error(ui, self.size_error.as_deref());
                ui.add_space(12.0);

                ui.checkbox(
                    &mut self.include_system,
                    "Also check system-wide caches and installed apps",
                );
                ui.label(
                    RichText::new("These don't depend on the folder above.")
                        .small()
                        .color(theme::palette(ui).muted),
                );
                ui.add_space(20.0);

                let scan = ui.add_sized(
                    [ui.available_width(), 38.0],
                    egui::Button::new(
                        RichText::new("Scan")
                            .strong()
                            .size(16.0)
                            .color(egui::Color32::WHITE),
                    )
                    .fill(theme::palette(ui).accent),
                );
                if scan.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.start_scan();
                }
            });
        });
    }

    fn scanning_screen(&self, ui: &mut Ui) {
        CentralPanel::default().show(ui, |ui| {
            centered_card(ui, 460.0, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add(
                        egui::Spinner::new()
                            .size(36.0)
                            .color(theme::palette(ui).accent),
                    );
                    ui.add_space(10.0);
                    ui.label(RichText::new("Scanning").heading());
                    ui.add(
                        egui::Label::new(
                            RichText::new(&self.scan_label).color(theme::palette(ui).muted),
                        )
                        .truncate(),
                    );
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new(format!(
                            "{} files checked",
                            self.progress.files_seen.load(Ordering::Relaxed)
                        ))
                        .size(20.0)
                        .family(theme::semibold()),
                    );
                    ui.label(
                        RichText::new(self.progress.phase_label())
                            .small()
                            .color(theme::palette(ui).muted),
                    );
                });
            });
        });
    }
}

fn spawn(job: impl FnOnce(std::sync::mpsc::Sender<BgMsg>) + Send + 'static) -> Receiver<BgMsg> {
    let (tx, rx) = channel();
    std::thread::spawn(move || job(tx));
    rx
}

/// Native folder picker, starting from `current` when it exists.
fn pick_folder(current: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new();
    let start = Path::new(current.trim());
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder()
}

fn quick_folders() -> Vec<(&'static str, PathBuf)> {
    let home = home_dir();
    [
        ("Home", home.clone()),
        ("Desktop", home.join("Desktop")),
        ("Downloads", home.join("Downloads")),
    ]
    .into_iter()
    .filter(|(_, p)| p.is_dir())
    .collect()
}

/// A card centered horizontally, a little above vertical center.
fn centered_card(ui: &mut Ui, width: f32, add_contents: impl FnOnce(&mut Ui)) {
    let top = ((ui.available_height() - 480.0) / 2.5).max(24.0);
    ui.add_space(top);
    ui.vertical_centered(|ui| {
        theme::card(ui).show(ui, |ui| {
            ui.set_width(width);
            ui.with_layout(Layout::top_down(Align::LEFT), add_contents);
        });
    });
}

fn field_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).family(theme::semibold()));
}

fn field_error(ui: &mut Ui, error: Option<&str>) {
    if let Some(e) = error {
        ui.label(RichText::new(e).small().color(theme::palette(ui).risky));
    }
}

impl eframe::App for ScavengerApp {
    /// Handle background results. No drawing allowed here — that's `ui`.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let Some(rx) = &self.rx else { return };
        match rx.try_recv() {
            Ok(msg) => {
                self.rx = None;
                self.finish(msg);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Keep the frame loop ticking so the counters move
                ctx.request_repaint_after(Duration::from_millis(150));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                self.acting = None;
                self.banner = Some((
                    BannerKind::Error,
                    "The background task stopped unexpectedly.".into(),
                ));
                if self.screen == Screen::Scanning {
                    self.screen = Screen::Start;
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        match self.screen {
            Screen::Start => self.start_screen(ui),
            Screen::Scanning => self.scanning_screen(ui),
            Screen::Results => results::show(self, ui),
        }
    }
}
