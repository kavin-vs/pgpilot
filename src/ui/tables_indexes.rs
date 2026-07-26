use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::format::{human_bytes, human_duration};
use crate::ui::{theme, widgets};

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.tables.is_none() {
        widgets::loading_or_error(frame, area, app, "tables", "Tables & Indexes");
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Percentage(52), Constraint::Percentage(45)])
        .split(area);

    draw_schema_totals(frame, rows[0], app);
    draw_tables(frame, rows[1], app);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[2]);
    draw_unused_indexes(frame, cols[0], app);
    draw_missing_indexes(frame, cols[1], app);
}

fn draw_schema_totals(frame: &mut Frame, area: Rect, app: &App) {
    let Some((data, _)) = &app.tables else { return };
    let text = data
        .schema_totals
        .iter()
        .map(|(schema, bytes)| format!("{schema}: {}", human_bytes(*bytes)))
        .collect::<Vec<_>>()
        .join("   ");

    let block = theme::block("Schema Totals");
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_tables(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some((data, _)) = &app.tables else { return };

    let header = Row::new(vec![
        "table", "rows", "size", "idx+toast", "seq scans/h", "idx use", "dead", "xid age", "last vacuum",
    ])
    .style(Style::default().fg(theme::TEXT_DIMMEST));

    let tables_rates = &app.tables_rates;
    let rows = data.tables.iter().map(|t| {
        let seq_rate = tables_rates
            .get(&(t.schema_name.clone(), t.table_name.clone()))
            .and_then(|r| r.seq_scan_per_hour)
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "—".to_string());
        let seq_color = if t.seq_scan >= 5000 {
            theme::BAD
        } else if t.seq_scan >= 500 {
            theme::WARN
        } else {
            theme::TEXT_DIM
        };
        let idx_use_pct = t.idx_use_pct.unwrap_or(100.0);
        let idx_color = if idx_use_pct < 50.0 {
            theme::BAD
        } else if idx_use_pct < 90.0 {
            theme::WARN
        } else {
            theme::TEXT_DIM
        };
        let dead_pct = t.dead_tuple_pct.unwrap_or(0.0);
        let dead_color = if dead_pct > 10.0 {
            theme::BAD
        } else if dead_pct > 5.0 {
            theme::WARN
        } else {
            theme::TEXT_DIM
        };
        let xid_color = if t.xid_age > 150_000_000 {
            theme::BAD
        } else if t.xid_age > 60_000_000 {
            theme::WARN
        } else {
            theme::TEXT_DIM
        };
        let last_vacuum = t
            .last_vacuum_secs_ago
            .map(human_duration)
            .unwrap_or_else(|| "never".to_string());

        Row::new(vec![
            Cell::from(format!("{}.{}", t.schema_name, t.table_name)),
            Cell::from(t.live_tuples.to_string()),
            Cell::from(human_bytes(t.total_bytes)),
            Cell::from(human_bytes(t.indexes_toast_bytes)),
            Cell::from(seq_rate).style(Style::default().fg(seq_color)),
            Cell::from(format!("{idx_use_pct:.1}%")).style(Style::default().fg(idx_color)),
            Cell::from(format!("{dead_pct:.1}%")).style(Style::default().fg(dead_color)),
            Cell::from(t.xid_age.to_string()).style(Style::default().fg(xid_color)),
            Cell::from(last_vacuum),
        ])
    });

    let widths = [
        Constraint::Percentage(22),
        Constraint::Percentage(9),
        Constraint::Percentage(9),
        Constraint::Percentage(10),
        Constraint::Percentage(11),
        Constraint::Percentage(9),
        Constraint::Percentage(8),
        Constraint::Percentage(10),
        Constraint::Percentage(12),
    ];

    let title = format!(
        "Tables (sorted by {} {})",
        app.tables_sort.label(),
        if app.tables_sort_dir == crate::app::SortDirection::Asc { "asc" } else { "desc" }
    );

    let table = Table::new(rows, widths)
        .header(header)
        .block(theme::block(title))
        .row_highlight_style(Style::default().bg(theme::ROW_SELECTED_BG));

    frame.render_stateful_widget(table, area, &mut app.tables_state);
}

fn draw_unused_indexes(frame: &mut Frame, area: Rect, app: &App) {
    let Some(indexes) = &app.indexes else {
        widgets::loading_or_error(frame, area, app, "indexes", "Unused Indexes");
        return;
    };
    // Invalid/not-ready indexes (e.g. a failed CREATE INDEX CONCURRENTLY)
    // surface here too, not just zero-scan ones — both are "this index isn't
    // pulling its weight" in different ways.
    let flagged: Vec<_> = indexes.iter().filter(|i| !i.is_valid || !i.is_ready || i.idx_scan == 0).collect();
    let reclaimable: i64 = flagged.iter().filter(|i| i.idx_scan == 0).map(|i| i.index_size_bytes).sum();

    let header = Row::new(vec!["index", "table", "size", "state"]).style(Style::default().fg(theme::TEXT_DIMMEST));
    let rows = flagged.iter().take(20).map(|i| {
        let (state, color) = if !i.is_valid || !i.is_ready {
            ("invalid/not-ready", theme::BAD)
        } else {
            ("unused", theme::WARN)
        };
        Row::new(vec![
            Cell::from(i.index_name.clone()),
            Cell::from(format!("{}.{}", i.schema_name, i.table_name)),
            Cell::from(human_bytes(i.index_size_bytes)),
            Cell::from(state).style(Style::default().fg(color)),
        ])
    });

    let widths = [
        Constraint::Percentage(35),
        Constraint::Percentage(30),
        Constraint::Percentage(15),
        Constraint::Percentage(20),
    ];
    let title = format!("Unused / Invalid Indexes · {} reclaimable", human_bytes(reclaimable));
    let table = Table::new(rows, widths).header(header).block(theme::block(title));
    frame.render_widget(table, area);
}

fn draw_missing_indexes(frame: &mut Frame, area: Rect, app: &App) {
    if app.unindexed_fks.is_none() {
        widgets::loading_or_error(frame, area, app, "unindexed foreign keys", "Missing Index Candidates");
        return;
    }
    let candidates = missing_index_candidates(app);
    let lines: Vec<Line> = if candidates.is_empty() {
        vec![Line::from("none found")]
    } else {
        candidates
            .iter()
            .map(|(marker, text, hint)| Line::from(format!("{marker} {text}\n   {hint}")))
            .collect()
    };

    let block = theme::block("Missing Index Candidates");
    frame.render_widget(Paragraph::new(lines).style(Style::default().fg(theme::TEXT)).block(block), area);
}

/// Two mechanically-derivable heuristics only (unindexed FKs, high seq-scan
/// ratio) — not the fabricated column-level DDL suggestions a query-plan
/// analyzer would produce. See plan/CLAUDE.md notes on this scoping.
fn missing_index_candidates(app: &App) -> Vec<(&'static str, String, String)> {
    let mut out = Vec::new();

    if let Some(fks) = &app.unindexed_fks {
        for fk in fks {
            out.push((
                "!",
                format!("{}.{} ({}) — unindexed foreign key", fk.schema_name, fk.table_name, fk.columns),
                format!("CREATE INDEX CONCURRENTLY ON {}.{} ({})", fk.schema_name, fk.table_name, fk.columns),
            ));
        }
    }

    if let Some((data, _)) = &app.tables {
        for t in &data.tables {
            if t.seq_scan >= 100 && t.idx_use_pct.unwrap_or(100.0) < 50.0 {
                out.push((
                    "·",
                    format!(
                        "{}.{} — {} seq scans, {:.1}% index use",
                        t.schema_name, t.table_name, t.seq_scan, t.idx_use_pct.unwrap_or(0.0)
                    ),
                    "review the table's common WHERE-clause columns for a supporting index".to_string(),
                ));
            }
        }
    }

    out.truncate(20);
    out
}
