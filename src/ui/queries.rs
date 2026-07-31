use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::db::statements::StatementsData;
use crate::ui::{charts, theme, widgets};

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.statements.is_none() {
        widgets::loading_or_error(frame, area, app, "pg_stat_statements", "Queries");
        return;
    }

    let is_not_available = matches!(&app.statements, Some((StatementsData::NotAvailable, _)));
    if is_not_available {
        draw_not_available(frame, area);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Percentage(35)])
        .split(area);

    draw_table(frame, rows[0], app);
    draw_detail(frame, rows[1], app);
}

fn draw_not_available(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::styled(
            "pg_stat_statements is not loaded — this view needs it",
            Style::default().fg(theme::WARN),
        ),
        Line::from(""),
        Line::from("Everything else in pgpilot works without it. Per-query time, call counts,"),
        Line::from("and per-query cache hit only come from this extension, which requires a"),
        Line::from("restart to load:"),
        Line::from(""),
        Line::from("  ALTER SYSTEM SET shared_preload_libraries = 'pg_stat_statements';"),
        Line::from("  -- restart postgres, then:"),
        Line::from("  CREATE EXTENSION pg_stat_statements;"),
        Line::from(""),
        Line::from("Meanwhile: Activity shows what's running now, Tables & Indexes finds seq"),
        Line::from("scans and unindexed foreign keys from catalog statistics alone."),
    ];
    let block = theme::block("Queries").border_style(Style::default().fg(theme::BORDER_DETAIL));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some((StatementsData::Available(stmt_rows), _)) = &app.statements else {
        return;
    };

    let header = Row::new(vec!["", "total s", "mean ms", "±stddev", "calls", "cache", "query"]).style(Style::default().fg(theme::TEXT_DIMMEST));
    let selected = app.queries_state.selected();
    let glyphs = charts::glyphs(app.ascii);

    let rows = stmt_rows.iter().enumerate().map(|(i, r)| {
        let cursor = if Some(i) == selected { glyphs.cursor } else { ' ' };
        let mean_color = if r.mean_exec_time_ms > 300.0 {
            theme::BAD
        } else if r.mean_exec_time_ms > 80.0 {
            theme::WARN
        } else {
            theme::TEXT
        };
        let cache_color = match r.cache_hit_pct {
            Some(p) if p < 90.0 => theme::BAD,
            Some(p) if p < 99.0 => theme::WARN,
            _ => theme::TEXT_DIM,
        };
        let cache = r.cache_hit_pct.map(|p| format!("{p:.1}%")).unwrap_or_else(|| "—".to_string());
        let row_bg = if Some(i) == selected { theme::ROW_SELECTED_BG } else { theme::BG };

        Row::new(vec![
            Cell::from(cursor.to_string()).style(Style::default().fg(theme::OK)),
            Cell::from(format!("{:.1}", r.total_exec_time_ms / 1000.0)),
            Cell::from(format!("{:.1}", r.mean_exec_time_ms)).style(Style::default().fg(mean_color)),
            Cell::from(format!("{:.1}", r.stddev_exec_time_ms)),
            Cell::from(r.calls.to_string()),
            Cell::from(cache).style(Style::default().fg(cache_color)),
            Cell::from(r.query.clone()),
        ])
        .style(Style::default().bg(row_bg))
    });

    let widths = [
        Constraint::Length(2),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Min(20),
    ];

    let title = format!("pg_stat_statements  (sorted by {} · s to change)", app.queries_sort.label());
    let table = Table::new(rows, widths)
        .header(header)
        .block(theme::block(title));

    frame.render_stateful_widget(table, area, &mut app.queries_state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App) {
    let Some((StatementsData::Available(stmt_rows), _)) = &app.statements else {
        return;
    };
    let selected = app.queries_state.selected().unwrap_or(0);
    let Some(row) = stmt_rows.get(selected) else {
        return;
    };

    let advice = advise(row);

    let lines = vec![
        Line::from(format!("queryid {}", row.query_id)),
        Line::from(row.query.clone()),
        Line::from(""),
        Line::from(format!(
            "total {:.1} s   mean {:.1} ms   ±stddev {:.1} ms   calls {}   rows/call {}   shared blks hit {}   read {}",
            row.total_exec_time_ms / 1000.0,
            row.mean_exec_time_ms,
            row.stddev_exec_time_ms,
            row.calls,
            if row.calls > 0 { row.rows / row.calls } else { 0 },
            row.shared_blks_hit,
            row.shared_blks_read,
        )),
        Line::from(""),
        Line::styled(format!("→ {advice}"), Style::default().fg(theme::TEXT_DIM)),
    ];

    let block = theme::block("Selected Statement").border_style(Style::default().fg(theme::BORDER_DETAIL));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn advise(row: &crate::db::statements::StatementRow) -> String {
    if row.cache_hit_pct.unwrap_or(100.0) < 90.0 {
        "low cache hit for this query — check for a large sequential scan or bump shared_buffers".to_string()
    } else if row.mean_exec_time_ms > 300.0 {
        "high mean latency — run EXPLAIN ANALYZE to check for a missing index or bad plan".to_string()
    } else if row.stddev_exec_time_ms > row.mean_exec_time_ms {
        "high variance run-to-run — likely plan instability or lock contention, not a steady cost".to_string()
    } else {
        "no obvious issue from these stats alone".to_string()
    }
}
