use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::format::human_duration;
use crate::ui::{charts, theme, widgets};

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.activity.is_none() {
        app.detail_pane_rect = None;
        app.table_pane_rect = None;
        widgets::loading_or_error(frame, area, app, "activity", "Activity");
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(6),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(area);

    app.detail_pane_rect = Some(rows[2]);
    app.table_pane_rect = Some(rows[1]);
    draw_summary_cards(frame, rows[0], app);
    draw_activity_table(frame, rows[1], app);
    draw_detail(frame, rows[2], app);
    draw_blocking_tree(frame, rows[3], app);
    draw_lock_strip(frame, rows[4], app);
}

/// Full-screen zoom of `draw_detail`, opened by clicking the inline pane
/// (`App::detail_popup_open`) — see `queries::draw_detail_popup`. Especially
/// useful here since this pane's fixed `Length(6)` height clips even a
/// modest query most of the time.
pub(crate) fn draw_detail_popup(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(ratatui::widgets::Clear, area);
    draw_detail(frame, area, app);
}

fn draw_summary_cards(frame: &mut Frame, area: Rect, app: &App) {
    let Some(activity) = &app.activity else { return };
    let waiting = activity.rows.iter().filter(|r| !r.blocked_by.is_empty()).count();

    // total/active/idle/idle-in-txn come from the same aggregate query the
    // old Connections panel used (`db::connections`) rather than re-deriving
    // them by scanning every activity row here.
    let (total, active, idle, idle_in_txn) = match &app.connections {
        Some(c) => (c.used, c.active, c.idle, c.idle_in_txn),
        None => (0, 0, 0, 0),
    };

    let cards = [
        ("total", total.to_string(), theme::TEXT),
        ("active", active.to_string(), theme::OK),
        ("idle", idle.to_string(), theme::TEXT_DIM),
        ("idle in txn", idle_in_txn.to_string(), if idle_in_txn > 0 { theme::WARN } else { theme::TEXT_DIM }),
        ("waiting on lock", waiting.to_string(), if waiting > 0 { theme::BAD } else { theme::TEXT_DIM }),
    ];

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(cards.iter().map(|_| Constraint::Percentage(20)).collect::<Vec<_>>())
        .split(area);

    for (i, (label, value, color)) in cards.iter().enumerate() {
        let text = vec![Line::from(*label), Line::from(value.as_str())];
        let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme::BORDER)).style(Style::default().bg(theme::PANEL_BG));
        frame.render_widget(Paragraph::new(text).style(Style::default().fg(*color)).block(block), cols[i]);
    }
}

fn draw_activity_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(activity) = &app.activity else { return };

    let header = Row::new(vec!["", "pid", "user", "state", "duration", "wait event", "query"]).style(Style::default().fg(theme::TEXT_DIMMEST));
    let selected = app.activity_state.selected();
    let glyphs = charts::glyphs(app.ascii);

    let rows = activity.rows.iter().enumerate().map(|(i, r)| {
        let cursor = if Some(i) == selected { glyphs.cursor } else { ' ' };
        let state = r.state.clone().unwrap_or_default();
        let state_color = match state.as_str() {
            "active" => theme::OK,
            "idle in transaction" | "idle in transaction (aborted)" => theme::BAD,
            _ => theme::TEXT_DIM,
        };
        let dur = r.duration_secs.map(human_duration).unwrap_or_default();
        let dur_color = match r.duration_secs {
            Some(d) if d > 60.0 => theme::BAD,
            Some(d) if d > 5.0 => theme::WARN,
            _ => theme::TEXT,
        };
        let wait = match (&r.wait_event_type, &r.wait_event) {
            (Some(t), Some(e)) => format!("{t}:{e}"),
            _ => "—".to_string(),
        };
        let row_bg = if Some(i) == selected { theme::ROW_SELECTED_BG } else { theme::BG };

        Row::new(vec![
            Cell::from(cursor.to_string()).style(Style::default().fg(theme::OK)),
            Cell::from(r.pid.to_string()),
            Cell::from(r.username.clone().unwrap_or_default()),
            Cell::from(state).style(Style::default().fg(state_color)),
            Cell::from(dur).style(Style::default().fg(dur_color)),
            Cell::from(wait),
            Cell::from(r.query.clone().unwrap_or_default()),
        ])
        .style(Style::default().bg(row_bg))
    });

    let widths = [
        Constraint::Length(2),
        Constraint::Length(8),
        Constraint::Percentage(12),
        Constraint::Percentage(14),
        Constraint::Percentage(10),
        Constraint::Percentage(18),
        Constraint::Percentage(30),
    ];

    let table = Table::new(rows, widths).header(header).block(theme::block("pg_stat_activity  (x: cancel · X: terminate backend)"));
    frame.render_stateful_widget(table, area, &mut app.activity_state);
}

/// Always-visible pane for whichever row is highlighted, matching
/// `queries.rs`'s `draw_detail` shape — not an `enter`-triggered popup. The
/// table's own query column is usually narrower than the 220-char
/// server-side cap (see `db::activity`), so this is where the full text
/// actually becomes readable; scrolls via `PageUp`/`PageDown`
/// (`widgets::draw_scrollable`) since this pane's fixed `Length(6)` height
/// clips even a modest query most of the time.
fn draw_detail(frame: &mut Frame, area: Rect, app: &App) {
    let Some(activity) = &app.activity else {
        return;
    };
    let selected = app.activity_state.selected().unwrap_or(0);
    let Some(row) = activity.rows.get(selected) else {
        return;
    };

    let wait = match (&row.wait_event_type, &row.wait_event) {
        (Some(t), Some(e)) => format!("{t}:{e}"),
        _ => "—".to_string(),
    };
    let dur = row.duration_secs.map(human_duration).unwrap_or_default();
    let text = row.query.clone().unwrap_or_else(|| "(no query text)".to_string());

    let mut lines = vec![
        Line::from(format!(
            "pid {}  user {}  state {}  duration {dur}  wait {wait}",
            row.pid,
            row.username.as_deref().unwrap_or("—"),
            row.state.as_deref().unwrap_or("—"),
        )),
        Line::from(""),
    ];
    lines.extend(text.lines().map(|l| Line::from(l.to_string())));

    widgets::draw_scrollable(frame, area, "Selected Backend", lines, app.detail_scroll);
}

fn draw_blocking_tree(frame: &mut Frame, area: Rect, app: &App) {
    let Some(activity) = &app.activity else { return };

    let blocked_rows: Vec<_> = activity.rows.iter().filter(|r| !r.blocked_by.is_empty()).collect();
    let mut blocker_pids: Vec<i32> = blocked_rows.iter().flat_map(|r| r.blocked_by.iter().copied()).collect();
    blocker_pids.sort_unstable();
    blocker_pids.dedup();

    let glyphs = charts::glyphs(app.ascii);
    let mut lines: Vec<Line> = Vec::new();
    if blocker_pids.is_empty() {
        lines.push(Line::from("no blocking sessions"));
    } else {
        for root_pid in blocker_pids {
            let root = activity.rows.iter().find(|r| r.pid == root_pid);
            let root_dur = root.and_then(|r| r.duration_secs).map(human_duration).unwrap_or_else(|| "?".to_string());
            let root_query = root.and_then(|r| r.query.clone()).unwrap_or_else(|| "(query unavailable)".to_string());
            lines.push(Line::styled(
                format!("{} {root_pid}  idle {root_dur} — {root_query}", glyphs.root),
                Style::default().fg(theme::BAD),
            ));
            for waiter in blocked_rows.iter().filter(|r| r.blocked_by.contains(&root_pid)) {
                let wait_dur = waiter.duration_secs.map(human_duration).unwrap_or_else(|| "?".to_string());
                lines.push(Line::styled(
                    format!("{} {}  waits {wait_dur} — {}", glyphs.branch, waiter.pid, waiter.query.clone().unwrap_or_default()),
                    Style::default().fg(theme::WARN),
                ));
            }
        }
    }

    let block = theme::block("Blocking Tree · the blocker is at the root — cancel that one, not the waiters");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_lock_strip(frame: &mut Frame, area: Rect, app: &App) {
    let Some(activity) = &app.activity else { return };
    let waiting = activity.rows.iter().filter(|r| !r.blocked_by.is_empty()).count();
    let longest = activity.rows.iter().filter_map(|r| r.duration_secs).fold(0.0_f64, f64::max);
    let idle_in_txn = activity
        .rows
        .iter()
        .filter(|r| matches!(r.state.as_deref(), Some("idle in transaction") | Some("idle in transaction (aborted)")))
        .count();

    let text = Line::from(format!(
        "locks waiting: {waiting}   longest running: {}   idle in transaction: {idle_in_txn}",
        human_duration(longest)
    ));
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme::BORDER)).style(Style::default().bg(theme::PANEL_BG));
    frame.render_widget(Paragraph::new(text).block(block), area);
}
