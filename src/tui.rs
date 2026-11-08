use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::io::{self, stdout};
use std::time::Duration;

use crate::actions::Target;
use crate::human_size;
use crate::scanner::ScanReport;
use crate::views::{DisplayItem, Safety, build_items, select_all_safe, selected_targets};

pub enum TuiAction {
    Offload(Vec<Target>),
    Clean(Vec<Target>),
    Quit,
}

/// Raw mode + alternate screen for as long as this lives. Dropping it (normal
/// exit, `?` early return, or unwinding panic) always gives the shell back.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(e) = execute!(stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(e);
        }
        // The default hook prints the panic message *before* unwinding reaches
        // Drop, so it would land on the alternate screen and vanish. Restore first.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen);
}

pub fn run_tui(report: &ScanReport) -> io::Result<TuiAction> {
    let mut items = build_items(report);
    let mut state = ListState::default();
    // Transient hint shown in the footer (e.g. action pressed with empty selection)
    let mut status: Option<String> = None;

    if !items.is_empty() {
        state.select(Some(0));
        move_cursor(&mut state, &items, 1); // land on the first non-header row
        if items.first().is_some_and(|i| !i.is_header) {
            state.select(Some(0));
        }
    }

    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    loop {
        terminal.draw(|f| draw(f, &items, &mut state, report, status.as_deref()))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // Letters match both cases — README and the footer show capitals
        match key.code {
            KeyCode::Esc => return Ok(TuiAction::Quit),
            KeyCode::Down => move_cursor(&mut state, &items, 1),
            KeyCode::Up => move_cursor(&mut state, &items, -1),
            KeyCode::PageDown => move_cursor(&mut state, &items, 10),
            KeyCode::PageUp => move_cursor(&mut state, &items, -10),
            KeyCode::Char(c) => match c.to_ascii_lowercase() {
                'q' => return Ok(TuiAction::Quit),
                'j' => move_cursor(&mut state, &items, 1),
                'k' => move_cursor(&mut state, &items, -1),
                ' ' => {
                    toggle_selection(&mut items, &state);
                    status = None;
                }
                'a' => {
                    select_all_safe(&mut items);
                    status = Some(
                        "Selected all [safe] rows. Tick [review]/[risky] rows yourself with Space."
                            .to_string(),
                    );
                }
                'n' => {
                    for item in &mut items {
                        item.selected = false;
                    }
                    status = None;
                }
                'o' | 'c' => {
                    let selected = selected_targets(&items);
                    if selected.is_empty() {
                        status = Some(
                            "No items selected — toggle with Space (or A for all safe rows)"
                                .to_string(),
                        );
                    } else if c.eq_ignore_ascii_case(&'o') {
                        return Ok(TuiAction::Offload(selected));
                    } else {
                        return Ok(TuiAction::Clean(selected));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn safety_color(s: Safety) -> Color {
    match s {
        Safety::Safe(_) => Color::Green,
        Safety::Review(_) => Color::Yellow,
        Safety::Risky(_) => Color::Red,
    }
}

#[allow(clippy::too_many_lines)] // one frame's layout, top to bottom
fn draw(
    f: &mut ratatui::Frame,
    items: &[DisplayItem],
    state: &mut ListState,
    report: &ScanReport,
    status: Option<&str>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Title bar
            Constraint::Min(5),    // Scrollable list
            Constraint::Length(7), // Details of the highlighted row
            Constraint::Length(2), // Keybindings + status
        ])
        .split(f.area());

    // --- Title bar ---
    let mut title = vec![
        Span::styled(
            " disk-scavenger",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  |  {} bloat",
            human_size(report.total_bloat_bytes)
        )),
        Span::raw(format!(
            "  |  {} files  {} caches  {} dup groups  {} apps",
            report.large_files.len(),
            report.caches.len(),
            report.duplicates.len(),
            report.heavy_apps.len()
        )),
    ];
    if report.skipped > 0 {
        title.push(Span::styled(
            format!("  |  {} unreadable", report.skipped),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(title)).block(Block::default().borders(Borders::ALL)),
        chunks[0],
    );

    // --- List ---
    let list_items: Vec<ListItem> = items
        .iter()
        .map(|item| {
            if item.is_header {
                return ListItem::new(Line::from(Span::styled(
                    item.label.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            let checkbox = if item.selected { "x" } else { " " };
            let base_style = if item.selected {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            // Verdict chip: what deleting this row does to the system
            let tag = item.safety.map_or_else(
                || Span::raw(""),
                |s| {
                    Span::styled(
                        format!(" [{}]", s.tag()),
                        Style::default().fg(safety_color(s)),
                    )
                },
            );
            ListItem::new(Line::from(vec![
                Span::styled(format!("[{checkbox}] "), base_style),
                Span::styled(item.label.clone(), base_style),
                tag,
            ]))
        })
        .collect();

    let list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Scan Results "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, chunks[1], state);

    // --- Details: exactly what acting on the highlighted row touches ---
    let detail_lines: Vec<Line> = state
        .selected()
        .and_then(|i| items.get(i))
        .filter(|i| !i.is_header)
        .map(|item| {
            let color = item.safety.map_or(Color::Reset, safety_color);
            item.details()
                .into_iter()
                .enumerate()
                .map(|(n, l)| {
                    if n == 0 {
                        Line::styled(l, Style::default().fg(color))
                    } else {
                        Line::raw(l)
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(detail_lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Details ")),
        chunks[2],
    );

    // --- Footer: two real lines (a "\n" inside a Span doesn't break the line) ---
    let selected_count = items.iter().filter(|i| i.selected).count();
    let selected_size: u64 = items.iter().filter(|i| i.selected).map(|i| i.size).sum();
    let second = match status {
        Some(msg) => Line::styled(format!(" {msg}"), Style::default().fg(Color::Yellow)),
        None => Line::raw(format!(
            " {selected_count} item(s) selected — {}",
            human_size(selected_size)
        )),
    };
    let footer = Text::from(vec![
        Line::styled(
            " ↑↓ Move  Space Toggle  A All safe  N None  O Offload  C Clean  Q Quit",
            Style::default().fg(Color::DarkGray),
        ),
        second,
    ]);
    f.render_widget(Paragraph::new(footer), chunks[3]);
}

/// Move the cursor by `delta`, skipping over header rows.
fn move_cursor(state: &mut ListState, items: &[DisplayItem], delta: isize) {
    if items.is_empty() {
        return;
    }
    let last = items.len() - 1;
    let current = state.selected().unwrap_or(0);
    let mut next = current.saturating_add_signed(delta).min(last);
    let step = delta.signum();

    // Skip headers in the direction of travel; if we hit the edge, try the other way
    while items[next].is_header {
        match next.checked_add_signed(step).filter(|&n| n <= last) {
            Some(n) => next = n,
            None => break,
        }
    }
    while items[next].is_header {
        match next.checked_add_signed(-step).filter(|&n| n <= last) {
            Some(n) => next = n,
            None => return, // nothing but headers
        }
    }
    state.select(Some(next));
}

fn toggle_selection(items: &mut [DisplayItem], state: &ListState) {
    if let Some(item) = state
        .selected()
        .and_then(|i| items.get_mut(i))
        .filter(|i| !i.is_header)
    {
        item.selected = !item.selected;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(headers: &[bool]) -> Vec<DisplayItem> {
        headers
            .iter()
            .map(|&h| DisplayItem {
                is_header: h,
                ..DisplayItem::default()
            })
            .collect()
    }

    #[test]
    fn test_cursor_skips_headers() {
        let list = items(&[true, false, true, false]);
        let mut s = ListState::default();
        s.select(Some(1));
        move_cursor(&mut s, &list, 1);
        assert_eq!(s.selected(), Some(3));
        move_cursor(&mut s, &list, -1);
        assert_eq!(s.selected(), Some(1));
        // At the top: header above, so stay put
        move_cursor(&mut s, &list, -1);
        assert_eq!(s.selected(), Some(1));
    }

    #[test]
    fn test_cursor_trailing_header_bounces_back() {
        // e.g. an empty "Duplicates (0 groups)" header at the bottom
        let list = items(&[true, false, true]);
        let mut s = ListState::default();
        s.select(Some(1));
        move_cursor(&mut s, &list, 10);
        assert_eq!(s.selected(), Some(1));
    }
}
