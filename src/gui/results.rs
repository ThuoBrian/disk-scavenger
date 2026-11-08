//! Results screen: category sidebar, the active category's rows, a detail
//! panel for the focused row, and the action bar along the bottom.

use std::fmt::Write as _;
use std::path::Path;

use disk_scavenger::actions::{self, Target};
use disk_scavenger::human_size;
use disk_scavenger::scanner::Category;
use disk_scavenger::views::{
    DisplayItem, Safety, category_info, category_totals, selected_targets,
};
use eframe::egui::{
    self, Align, CentralPanel, Checkbox, CornerRadius, CursorIcon, Frame, Id, Label, Layout,
    Margin, Panel, RichText, ScrollArea, Sense, Ui, UiBuilder,
};

use crate::{BannerKind, ScavengerApp, Screen, pick_folder, theme};

/// Width of the right-aligned size column in the row list
const SIZE_COL: f32 = 84.0;

pub fn show(app: &mut ScavengerApp, ui: &mut Ui) {
    let totals = category_totals(&app.items);
    // Land on the first non-empty category, and move off one that emptied out
    if !totals.iter().any(|(c, ..)| Some(*c) == app.active) {
        app.active = totals.first().map(|(c, ..)| *c);
    }

    // Side and bottom panels must be added before the central panel
    action_bar(app, ui);
    sidebar(app, ui, &totals);
    detail_panel(app, ui);
    CentralPanel::default()
        .frame(
            Frame::new()
                .inner_margin(Margin::symmetric(20, 16))
                .fill(ui.visuals().panel_fill),
        )
        .show(ui, |ui| center(app, ui, &totals));

    if app.show_clean_modal {
        clean_modal(app, ui);
    }
}

/// Banner in the theme's colors. Returns true if dismissed.
pub fn banner(ui: &mut Ui, kind: BannerKind, text: &str) -> bool {
    let p = theme::palette(ui);
    let color = match kind {
        BannerKind::Success => p.safe,
        BannerKind::Warning => p.review,
        BannerKind::Error => p.risky,
    };
    theme::banner(ui, color, text, true)
}

fn side_frame(ui: &Ui) -> Frame {
    Frame::new()
        .fill(theme::palette(ui).surface)
        .inner_margin(Margin::same(16))
}

// ----------------------------------------------------------------- sidebar

fn sidebar(app: &mut ScavengerApp, ui: &mut Ui, totals: &[(Category, usize, u64)]) {
    let frame = side_frame(ui);
    Panel::left("categories")
        .resizable(false)
        .exact_size(240.0)
        .frame(frame)
        .show(ui, |ui| {
            let p = theme::palette(ui);
            let grand: u64 = totals.iter().map(|(.., s)| s).sum();
            let rows: usize = totals.iter().map(|(_, n, _)| n).sum();

            ui.label(RichText::new("RECLAIMABLE").small().color(p.muted));
            ui.label(
                RichText::new(human_size(grand))
                    .size(30.0)
                    .family(theme::semibold()),
            );
            ui.label(
                RichText::new(format!("across {rows} items"))
                    .small()
                    .color(p.muted),
            );
            ui.add_space(14.0);

            for &(cat, count, size) in totals {
                category_entry(app, ui, cat, count, size, grand);
                ui.add_space(4.0);
            }

            ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                let busy = app.acting.is_some();
                if theme::secondary_button(ui, "← New scan", !busy).clicked() {
                    app.screen = Screen::Start;
                    app.items.clear();
                    app.banner = None;
                }
                if app.skipped > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{} entries couldn't be read and were skipped.",
                            app.skipped
                        ))
                        .small()
                        .color(p.muted),
                    );
                }
            });
        });
}

fn category_entry(
    app: &mut ScavengerApp,
    ui: &mut Ui,
    cat: Category,
    count: usize,
    size: u64,
    grand: u64,
) {
    let p = theme::palette(ui);
    let (name, usual) = category_info(cat);
    let active = app.active == Some(cat);
    let ticked = app
        .items
        .iter()
        .filter(|i| i.selected && i.category == Some(cat))
        .count();

    let fill = if active {
        p.accent.gamma_multiply(0.16)
    } else {
        egui::Color32::TRANSPARENT
    };
    let response = ui
        .scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
            Frame::new()
                .fill(fill)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::Sides::new().show(
                        ui,
                        |ui| {
                            ui.label(RichText::new(name).family(theme::semibold()));
                        },
                        |ui| {
                            ui.label(RichText::new(human_size(size)).color(p.muted));
                        },
                    );
                    let fraction = if grand == 0 {
                        0.0
                    } else {
                        size as f32 / grand as f32
                    };
                    theme::share_bar(ui, fraction, p.safety(usual));
                    let mut meta = format!("{count} item{}", if count == 1 { "" } else { "s" });
                    if ticked > 0 {
                        let _ = write!(meta, " · {ticked} ticked");
                    }
                    ui.label(RichText::new(meta).small().color(p.muted));
                })
        })
        .response
        .on_hover_cursor(CursorIcon::PointingHand);
    if response.clicked() {
        app.active = Some(cat);
    }
}

// ------------------------------------------------------------------ center

fn center(app: &mut ScavengerApp, ui: &mut Ui, totals: &[(Category, usize, u64)]) {
    let p = theme::palette(ui);

    if let Some(label) = &app.acting {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(label).color(p.accent));
        });
        ui.add_space(6.0);
    }
    if let Some((kind, text)) = &app.banner
        && banner(ui, *kind, text)
    {
        app.banner = None;
    }

    let Some(cat) = app.active else {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Nothing to clean up").heading());
            ui.label(
                RichText::new("The scan found no large files, duplicates or caches.")
                    .color(p.muted),
            );
        });
        return;
    };
    let (name, usual) = category_info(cat);
    let (count, size) = totals
        .iter()
        .find(|(c, ..)| *c == cat)
        .map_or((0, 0), |&(_, n, s)| (n, s));

    // Header: name + totals on the left, per-category selection on the right
    let busy = app.acting.is_some();
    egui::Sides::new().show(
        ui,
        |ui| {
            ui.label(RichText::new(name).heading());
            ui.label(RichText::new(format!("{count} · {}", human_size(size))).color(p.muted));
        },
        |ui| {
            if ui.add_enabled(!busy, egui::Button::new("Clear")).clicked() {
                for i in app.items.iter_mut().filter(|i| i.category == Some(cat)) {
                    i.selected = false;
                }
            }
            let any_safe = app
                .items
                .iter()
                .any(|i| i.category == Some(cat) && matches!(i.safety, Some(Safety::Safe(_))));
            if ui
                .add_enabled(!busy && any_safe, egui::Button::new("Select all safe"))
                .on_disabled_hover_text("Rows here need a decision one by one")
                .clicked()
            {
                for i in app.items.iter_mut().filter(|i| i.category == Some(cat)) {
                    i.selected = matches!(i.safety, Some(Safety::Safe(_)));
                }
            }
        },
    );
    ui.add_space(4.0);
    // One explanation per category instead of a reason on every row
    theme::banner(ui, p.safety(usual), usual.reason(), false);
    ui.add_space(8.0);

    let indices: Vec<usize> = app
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| !i.is_header && i.category == Some(cat))
        .map(|(n, _)| n)
        .collect();

    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for (stripe, &idx) in indices.iter().enumerate() {
            let focused = app.focused == Some(idx);
            if row(
                ui,
                &mut app.items[idx],
                usual,
                focused,
                stripe % 2 == 1,
                busy,
            ) {
                // Clicking anywhere on a row ticks it, like the checkbox, and shows
                // its details. Before, a row click only highlighted it, which looked
                // like a selection but left nothing ticked to delete.
                app.focused = Some(idx);
                if !busy {
                    app.items[idx].selected ^= true;
                }
            }
        }
    });
}

/// One two-line row. Returns true if the row body (not its checkbox) was clicked;
/// the caller toggles the selection and focuses the row.
fn row(
    ui: &mut Ui,
    item: &mut DisplayItem,
    usual: Safety,
    focused: bool,
    striped: bool,
    busy: bool,
) -> bool {
    let p = theme::palette(ui);
    let fill = if focused {
        p.accent.gamma_multiply(0.18)
    } else if striped {
        ui.visuals().faint_bg_color
    } else {
        egui::Color32::TRANSPARENT
    };
    let full_path = item
        .target
        .as_ref()
        .and_then(|t| t.roots().first().map(|p| p.display().to_string()))
        .unwrap_or_default();

    // UiBuilder::sense registers the row's click area *below* the widgets inside
    // it, so the checkbox keeps its own clicks and the rest of the row focuses it.
    // (Calling .interact() on the frame afterwards put it on top and ate them.)
    let inner = ui.scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
        Frame::new()
            .fill(fill)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add_enabled(!busy, Checkbox::without_text(&mut item.selected));
                    ui.add_sized(
                        [SIZE_COL, 20.0],
                        Label::new(RichText::new(human_size(item.size)).monospace().color(
                            if item.selected {
                                p.accent
                            } else {
                                ui.visuals().text_color()
                            },
                        ))
                        .selectable(false),
                    );
                    // Chip only where the row breaks from its category's usual verdict,
                    // and always on risky rows
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(s) = item.safety
                            && (s.tag() != usual.tag() || matches!(s, Safety::Risky(_)))
                        {
                            theme::chip(ui, s.tag(), p.safety(s));
                        }
                        ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
                            ui.add(
                                Label::new(RichText::new(&item.name).family(theme::semibold()))
                                    .truncate()
                                    .selectable(false),
                            )
                            .on_hover_text(&full_path);
                            ui.add(
                                Label::new(RichText::new(&item.location).small().color(p.muted))
                                    .truncate()
                                    .selectable(false),
                            )
                            .on_hover_text(&full_path);
                        });
                    });
                });
            });
    });
    inner
        .response
        .on_hover_cursor(CursorIcon::PointingHand)
        .clicked()
}

// ------------------------------------------------------------ detail panel

fn detail_panel(app: &mut ScavengerApp, ui: &mut Ui) {
    let frame = side_frame(ui);
    Panel::right("details")
        .resizable(true)
        .default_size(300.0)
        .min_size(220.0)
        .frame(frame)
        .show(ui, |ui| {
            let p = theme::palette(ui);
            ui.label(RichText::new("DETAILS").small().color(p.muted));
            ui.add_space(6.0);

            let Some(item) = app.focused.and_then(|i| app.items.get(i)) else {
                ui.label(
                    RichText::new("Click a row to see exactly which files it affects.")
                        .color(p.muted),
                );
                return;
            };
            ui.add(
                Label::new(
                    RichText::new(&item.name)
                        .size(16.0)
                        .family(theme::semibold()),
                )
                .wrap(),
            );
            ui.label(
                RichText::new(human_size(item.size))
                    .size(22.0)
                    .family(theme::semibold()),
            );
            if let Some(s) = item.safety {
                ui.add_space(4.0);
                theme::chip(ui, s.tag(), p.safety(s));
                ui.add(Label::new(RichText::new(s.reason()).color(p.muted)).wrap());
            }
            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new("Affected").family(theme::semibold()));

            let Some(target) = item.target.clone() else {
                return;
            };
            ScrollArea::vertical()
                .max_height(ui.available_height() - 60.0)
                .show(ui, |ui| {
                    for line in target.describe() {
                        let color = if line.starts_with("keep") {
                            p.safe
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.add(
                            Label::new(RichText::new(line).monospace().size(12.0).color(color))
                                .wrap(),
                        );
                    }
                });
            ui.add_space(8.0);
            if let Some(first) = target.roots().first()
                && theme::secondary_button(ui, REVEAL_LABEL, true).clicked()
                && let Err(e) = reveal(first)
            {
                app.banner = Some((BannerKind::Error, format!("Couldn't open the folder: {e}")));
            }
        });
}

#[cfg(target_os = "windows")]
const REVEAL_LABEL: &str = "Show in Explorer";
#[cfg(target_os = "macos")]
const REVEAL_LABEL: &str = "Show in Finder";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const REVEAL_LABEL: &str = "Open folder";

/// Open the system file manager with `path` selected (or its folder opened).
fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        // explorer wants "/select,<path>" as one argument
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path.as_os_str());
        c.arg(arg);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(path);
        c
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    };
    // explorer.exe exits 1 even on success, so only spawning can fail here
    cmd.spawn().map(drop)
}

// -------------------------------------------------------------- action bar

fn action_bar(app: &mut ScavengerApp, ui: &mut Ui) {
    let frame = side_frame(ui).inner_margin(Margin::symmetric(16, 10));
    Panel::bottom("actions").frame(frame).show(ui, |ui| {
        let p = theme::palette(ui);
        let count = app.items.iter().filter(|i| i.selected).count();
        let size: u64 = app
            .items
            .iter()
            .filter(|i| i.selected)
            .map(|i| i.size)
            .sum();
        let ready = count > 0 && app.acting.is_none();

        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{count} selected")).family(theme::semibold()));
            ui.label(RichText::new(human_size(size)).color(p.muted));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let trash = format!("Move to {}", actions::TRASH_NAME);
                if theme::danger_button(ui, &trash, ready).clicked() {
                    app.show_clean_modal = true;
                }
                if theme::secondary_button(ui, "Offload", ready)
                    .on_hover_text(
                        "Copy the selected files to the destination and verify each copy",
                    )
                    .clicked()
                {
                    app.start_offload();
                }
                if theme::secondary_button(ui, "Browse…", app.acting.is_none()).clicked()
                    && let Some(d) = pick_folder(&app.dest)
                {
                    app.dest = d.display().to_string();
                }
                ui.add(
                    egui::TextEdit::singleline(&mut app.dest)
                        .hint_text("Offload destination, e.g. E:\\backup")
                        .desired_width(260.0),
                );
            });
        });
    });
}

// ------------------------------------------------------------- clean modal

fn clean_modal(app: &mut ScavengerApp, ui: &mut Ui) {
    let targets = selected_targets(&app.items);
    let size: u64 = app
        .items
        .iter()
        .filter(|i| i.selected)
        .map(|i| i.size)
        .sum();
    let mut confirm = false;
    let mut cancel = false;

    let modal = egui::Modal::new(Id::unique("confirm-clean")).show(ui.ctx(), |ui| {
        let p = theme::palette(ui);
        ui.set_width(480.0);
        ui.label(
            RichText::new(format!(
                "Move {} item(s) to the {}?",
                targets.len(),
                actions::TRASH_NAME
            ))
            .heading(),
        );
        ui.label(
            RichText::new(format!("About {} will be moved.", human_size(size))).color(p.muted),
        );
        ui.add_space(8.0);

        let lines: Vec<String> = targets.iter().flat_map(removed_lines).collect();
        Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for line in lines.iter().take(6) {
                    ui.add(Label::new(RichText::new(line).monospace().size(12.0)).truncate());
                }
                if lines.len() > 6 {
                    ui.label(
                        RichText::new(format!("…and {} more", lines.len() - 6))
                            .small()
                            .color(p.muted),
                    );
                }
            });
        ui.add_space(8.0);
        for note in actions::trash_notes(size) {
            theme::banner(ui, p.review, &note, false);
            ui.add_space(4.0);
        }
        ui.add_space(8.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            confirm = theme::danger_button(ui, "Move to trash", true).clicked();
            cancel = theme::secondary_button(ui, "Cancel", true).clicked();
        });
    });

    if confirm {
        app.start_clean();
    } else if cancel || modal.should_close() {
        app.show_clean_modal = false;
    }
}

/// The paths a clean would actually move (kept duplicate copies excluded).
fn removed_lines(target: &Target) -> Vec<String> {
    match target {
        Target::Duplicate { redundant, .. } => {
            redundant.iter().map(|p| p.display().to_string()).collect()
        }
        Target::CacheContents(dir) => vec![format!("contents of {}", dir.display())],
        Target::Path(p) => vec![p.display().to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Context, Event, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};

    /// Render one row, returning (row clicked, row rect).
    fn render(ctx: &Context, events: Vec<Event>, item: &mut DisplayItem) -> (bool, Rect) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 200.0))),
            events,
            ..RawInput::default()
        };
        let mut out = (false, Rect::NOTHING);
        let mut output = ctx.run_ui(input, |ui| {
            let scoped = ui.scope(|ui| row(ui, item, Safety::Review(""), false, false, false));
            out = (scoped.inner, scoped.response.rect);
        });
        // No renderer here, so discard the font texture uploads ourselves
        output.textures_delta.clear();
        out
    }

    fn click(ctx: &Context, item: &mut DisplayItem, pos: Pos2) -> bool {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        render(ctx, vec![Event::PointerMoved(pos), button(true)], item);
        let (clicked, _) = render(ctx, vec![button(false)], item);
        render(ctx, vec![], item);
        clicked
    }

    #[test]
    fn test_checkbox_selects_and_row_click_only_focuses() {
        let ctx = Context::default();
        crate::theme::apply(&ctx);
        let mut item = DisplayItem {
            name: "big.iso".into(),
            location: r"C:\Users\me\Downloads".into(),
            size: 1 << 30,
            ..DisplayItem::default()
        };
        // Lay out once so the rects are known
        render(&ctx, vec![], &mut item);
        let (_, rect) = render(&ctx, vec![], &mut item);

        // The checkbox sits just inside the row's left padding
        let checkbox = Pos2::new(rect.left() + 10.0 + 8.0, rect.center().y);
        let row_clicked = click(&ctx, &mut item, checkbox);
        assert!(item.selected, "clicking the checkbox must tick the row");
        assert!(
            !row_clicked,
            "the checkbox click must not also count as a row click"
        );

        // Clicking the name focuses the row and leaves the selection alone
        let name = Pos2::new(rect.left() + 10.0 + 28.0 + SIZE_COL + 40.0, rect.center().y);
        let row_clicked = click(&ctx, &mut item, name);
        assert!(row_clicked, "clicking the row body must focus it");
        assert!(item.selected, "focusing must not untick");
    }

    /// Results screen with two large files, as the user sees it.
    fn app() -> crate::ScavengerApp {
        use disk_scavenger::scanner::{FileEntry, ScanReport};
        let file = |p: &str| FileEntry {
            path: p.into(),
            size: 2 << 30,
            modified: String::new(),
            category: Category::LargeFile,
            review: false,
        };
        let report = ScanReport {
            scan_path: String::new(),
            scan_time: String::new(),
            total_bloat_bytes: 0,
            skipped: 0,
            large_files: vec![file(r"C:\one.iso"), file(r"C:	wo.iso")],
            caches: vec![],
            duplicates: vec![],
            heavy_apps: vec![],
        };
        let mut app = crate::ScavengerApp::new();
        app.items = disk_scavenger::views::build_items(&report);
        app.screen = Screen::Results;
        app
    }

    fn render_app(ctx: &Context, events: Vec<Event>, app: &mut crate::ScavengerApp) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 660.0))),
            events,
            ..RawInput::default()
        };
        let mut output = ctx.run_ui(input, |ui| show(app, ui));
        output.textures_delta.clear();
    }

    fn click_app(ctx: &Context, app: &mut crate::ScavengerApp, pos: Pos2) {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        render_app(ctx, vec![Event::PointerMoved(pos), button(true)], app);
        render_app(ctx, vec![button(false)], app);
        render_app(ctx, vec![Event::PointerGone], app);
    }

    #[test]
    fn test_rows_can_be_ticked_on_the_full_results_screen() {
        let ctx = Context::default();
        crate::theme::apply(&ctx);
        let mut app = app();
        render_app(&ctx, vec![], &mut app);
        render_app(&ctx, vec![], &mut app);

        // Walk down the checkbox column (sidebar 240 + padding) until a click ticks a row
        let checkbox_x = 240.0 + 20.0 + 10.0 + 9.0;
        let ticked_at = (0..160).map(|i| 60.0 + i as f32 * 3.0).find(|&y| {
            click_app(&ctx, &mut app, Pos2::new(checkbox_x, y));
            app.items.iter().any(|i| i.selected)
        });
        let y = ticked_at.expect("no click in the checkbox column ticked a row");
        assert_eq!(app.items.iter().filter(|i| i.selected).count(), 1);

        // Clicking the same row's name unticks it again and keeps it focused
        let name_x = checkbox_x + 30.0 + SIZE_COL + 40.0;
        click_app(&ctx, &mut app, Pos2::new(name_x, y));
        assert_eq!(app.items.iter().filter(|i| i.selected).count(), 0);
        assert!(app.focused.is_some());
    }

    #[test]
    fn test_clicking_a_row_ticks_it() {
        let ctx = Context::default();
        crate::theme::apply(&ctx);
        let mut app = app();
        render_app(&ctx, vec![], &mut app);
        render_app(&ctx, vec![], &mut app);

        // Click down the name column (well right of the checkbox) until a row ticks
        let name_x = 240.0 + 20.0 + 10.0 + 30.0 + SIZE_COL + 60.0;
        let ticked = (0..160).map(|i| 60.0 + i as f32 * 3.0).any(|y| {
            click_app(&ctx, &mut app, Pos2::new(name_x, y));
            app.items.iter().any(|i| i.selected)
        });
        assert!(ticked, "clicking a row's name must tick it");
        let focused = app.focused.expect("the clicked row is focused");
        assert!(
            app.items[focused].selected,
            "the ticked row is the focused one"
        );
    }
}
