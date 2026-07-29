use std::time::Duration;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
    Frame,
};

use crate::app::{App, PanelKind};
use crate::event::StatusLevel;
use crate::format::{human_bytes, human_duration, human_rate};
use crate::ui::{charts, theme};

pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// How long a transient footer status (refreshed, sorted by..., paused,
/// etc.) stays visible before the footer reverts to the help text — without
/// this it sticks forever, permanently hiding the keybinding hints.
const STATUS_TTL: Duration = Duration::from_secs(3);

pub fn spinner(frame: usize) -> char {
    SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
}

/// Shared "waiting on the first query result" panel, used by every dashboard
/// block before its first snapshot has arrived.
pub fn loading(frame: &mut Frame, area: Rect, title: &str, spinner_frame: usize) {
    let text = format!("{}  Loading {title}...", spinner(spinner_frame));
    let widget = Paragraph::new(text)
        .style(Style::default().fg(theme::OK))
        .block(theme::block(title));
    frame.render_widget(widget, area);
}

/// Shared "this block's query failed" panel — a red-bordered block with an
/// error symbol in the title and a short message inside, so a broken block
/// reads as *broken*, not as perpetually "still loading". Full error text is
/// always reachable via `e`.
pub fn error_block(frame: &mut Frame, area: Rect, title: &str, message: &str) {
    let widget = Paragraph::new(format!("{message}\n\n(e: full error)"))
        .style(Style::default().fg(theme::BAD))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .block(
            Block::default()
                .title(format!("✗ {title}"))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BAD))
                .style(Style::default().bg(theme::PANEL_BG)),
        );
    frame.render_widget(widget, area);
}

/// The standard "no data yet" state for a block backed by `source_label`:
/// `error_block` if that source has a sticky error, otherwise the normal
/// `loading` spinner. Distinguishes "broken" from "still loading" per block,
/// rather than only via the shared footer/detail-view error surfacing.
pub fn loading_or_error(frame: &mut Frame, area: Rect, app: &App, source_label: &str, title: &str) {
    match app.errors.get(source_label) {
        Some(message) => error_block(frame, area, title, message),
        None => loading(frame, area, title, app.spinner_frame),
    }
}

/// Persistent top info bar: brand, current db, host:port, PG version, size,
/// owner, uptime, live/paused indicator.
pub fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let sep = || Span::styled(" │ ", Style::default().fg(theme::TEXT_DIMMEST));
    let dim = |s: String| Span::styled(s, Style::default().fg(theme::TEXT_DIMMER));

    let mut spans = vec![
        Span::styled("pgpilot", Style::default().fg(theme::OK).add_modifier(Modifier::BOLD)),
        sep(),
        Span::styled(
            app.current_dbname.clone().unwrap_or_else(|| "-".to_string()),
            Style::default().fg(theme::TEXT_BRIGHT).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(addr) = &app.server_addr {
        spans.push(dim(format!(" @ {addr}")));
    }
    if let Some(info) = &app.server_info {
        spans.push(sep());
        spans.push(dim(format!("PostgreSQL {}", info.version)));
        spans.push(sep());
        spans.push(dim(format!("owner {}", info.current_db_owner)));
    }
    if let Some((dbs, _)) = &app.databases
        && let Some(cur) = dbs.iter().find(|d| Some(d.name.as_str()) == app.current_dbname.as_deref())
    {
        spans.push(sep());
        spans.push(dim(human_bytes(cur.size_bytes)));
    }
    if let Some(info) = &app.server_info {
        spans.push(sep());
        spans.push(dim(format!("up {}", human_duration(info.uptime_secs))));
    }
    spans.push(sep());
    spans.push(dim(format!("sample {}", human_rate(app.rate))));
    spans.push(sep());

    let glyphs = charts::glyphs(app.ascii);
    let (indicator, label, color) = if app.paused {
        (glyphs.pause.to_string(), "paused", theme::WARN)
    } else {
        (glyphs.dot.to_string(), "live", theme::OK)
    };
    spans.push(Span::styled(format!("{indicator} {label}"), Style::default().fg(color)));

    let widget = Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::PANEL_BG).fg(theme::TEXT));
    frame.render_widget(widget, area);
}

pub fn draw_tab_bar(frame: &mut Frame, area: Rect, active: PanelKind) {
    let titles: Vec<Line> = PanelKind::ALL
        .iter()
        .enumerate()
        .map(|(i, p)| Line::from(format!("[{}] {}", i + 1, p.title())))
        .collect();
    let selected = PanelKind::ALL.iter().position(|p| *p == active).unwrap_or(0);

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme::BORDER)).style(Style::default().bg(theme::PANEL_BG_ALT)))
        .style(Style::default().fg(theme::TEXT_DIMMER).bg(theme::PANEL_BG_ALT))
        .select(selected)
        .highlight_style(
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .bg(theme::PANEL_BG)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, area);
}

pub fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let updated = match app.last_refresh {
        Some(t) => format!("  |  updated {}s ago", t.elapsed().as_secs()),
        None => "  |  no data yet".to_string(),
    };

    // Only advertise keys that actually do something on the active tab —
    // mirrors app.rs's active_row_count/cycle_sort/cancel_or_terminate.
    let has_rows = matches!(
        app.active,
        PanelKind::Queries | PanelKind::Activity | PanelKind::TablesIndexes | PanelKind::Triggers
    );
    let has_sort = matches!(app.active, PanelKind::Queries | PanelKind::TablesIndexes);

    let mut help = "q: quit  1-6: view".to_string();
    if has_rows {
        help.push_str("  j/k: move");
    }
    if has_sort {
        help.push_str("  s: sort");
    }
    if app.active == PanelKind::Activity {
        help.push_str("  x/X: cancel/terminate");
    }
    help.push_str("  r: refresh  space: pause");
    if app.active == PanelKind::Triggers {
        help.push_str("  enter: view function");
    }
    if app.can_switch_db {
        help.push_str("  d: database");
    }
    help.push_str(&format!("  -/+: rate {}", human_rate(app.rate)));

    let status = app.status.as_ref().filter(|(_, _, at)| at.elapsed() < STATUS_TTL);

    let (text, style) = if let Some((source, message)) = app.errors.iter().next() {
        let summary = if app.errors.len() == 1 {
            format!("ERROR: {source}: {message}")
        } else {
            let sources: Vec<&str> = app.errors.keys().map(String::as_str).collect();
            format!("ERROR: {} issues — {}", app.errors.len(), sources.join(", "))
        };
        (format!("{summary}  (e: full error){updated}"), Style::default().fg(theme::BAD))
    } else if let Some((status, level, _)) = status {
        let color = if *level == StatusLevel::Warn { theme::WARN } else { theme::OK };
        (format!("{status}{updated}"), Style::default().fg(color))
    } else {
        (format!("{help}{updated}"), Style::default().fg(theme::TEXT_DIMMER))
    };

    frame.render_widget(Paragraph::new(text).style(style), area);
}

/// Full-screen overlay listing every current sticky error as a numbered
/// point (source, then its full message indented on the next line) — the
/// footer's single line truncates long Postgres error messages and can only
/// name one source at a time. Opened with `e`, closed with `e`/`esc`/`q`.
pub fn draw_error_detail(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(ratatui::widgets::Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    for (i, (source, message)) in app.errors.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(format!("{}. ", i + 1), Style::default().fg(theme::BAD).add_modifier(Modifier::BOLD)),
            Span::styled(source.clone(), Style::default().fg(theme::TEXT_BRIGHT).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(vec![Span::raw("   "), Span::styled(message.clone(), Style::default().fg(theme::BAD))]));
        lines.push(Line::raw(""));
    }
    if lines.is_empty() {
        lines.push(Line::from("no errors"));
    }

    let block = Block::default()
        .title(format!("Error detail — {} issue(s)  (e / esc / q to close)", app.errors.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BAD))
        .style(Style::default().bg(theme::PANEL_BG));
    let widget = Paragraph::new(lines)
        .block(block)
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(widget, area);
}
