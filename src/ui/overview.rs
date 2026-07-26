use std::collections::VecDeque;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::db::statements::StatementsData;
use crate::diagnosis::{self, DiagnosisInputs, Severity};
use crate::ui::{charts, theme, widgets};

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    if app.connections.is_none() && app.cache_overall.is_none() && app.activity.is_none() {
        widgets::loading(frame, area, "Overview", app.spinner_frame);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Length(6), Constraint::Percentage(35), Constraint::Percentage(30)])
        .split(area);

    draw_diagnosis(frame, rows[0], app);
    draw_cards(frame, rows[1], app);

    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[2]);
    draw_throughput(frame, mid[0], app);
    draw_wait_events(frame, mid[1], app);

    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[3]);
    draw_top_statements(frame, bottom[0], app);
    draw_alerts(frame, bottom[1], app);
}

fn build_inputs(app: &App) -> DiagnosisInputs<'_> {
    DiagnosisInputs {
        tables: app.tables.as_ref().map(|(d, _)| d),
        indexes: app.indexes.as_deref(),
        unindexed_fks: app.unindexed_fks.as_deref().unwrap_or(&[]),
        activity: app.activity.as_ref(),
        statements: app.statements.as_ref().map(|(d, _)| d),
        cache: app.cache_overall.as_ref().map(|(d, _)| d),
    }
}

fn severity_color(s: Severity) -> Color {
    match s {
        Severity::Warn => theme::WARN,
        Severity::Bad => theme::BAD,
    }
}

fn draw_diagnosis(frame: &mut Frame, area: Rect, app: &App) {
    let inputs = build_inputs(app);
    let diag = diagnosis::diagnose(&inputs);

    let mut lines = vec![Line::styled(diag.headline, Style::default().fg(theme::TEXT_BRIGHT))];
    for s in &diag.suspects {
        let color = severity_color(s.severity);
        lines.push(Line::from(vec![
            Span::styled(format!("{}. ", s.rank), Style::default().fg(color)),
            Span::styled(format!("{:<22}", s.kind), Style::default().fg(theme::TEXT)),
            Span::styled(s.text.clone(), Style::default().fg(theme::TEXT_DIM)),
            Span::raw("  "),
            Span::styled(s.evidence.clone(), Style::default().fg(theme::TEXT_DIMMEST)),
        ]));
    }

    let block = theme::block("Diagnosis").border_style(Style::default().fg(theme::BORDER_DIAGNOSIS));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_cards(frame: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25); 4])
        .split(area);

    let max_conn = app.connections.as_ref().map(|c| c.max_connections).unwrap_or(0);

    render_card(frame, cols[0], app, "transactions", &app.history.tps, "/s", theme::TEXT, |v| format!("{v:.0}"));
    render_card(frame, cols[1], app, "connections", &app.history.conn, &format!("/ {max_conn}"), conn_color(&app.history.conn, max_conn), |v| format!("{v:.0}"));
    render_card(frame, cols[2], app, "cache hit", &app.history.cache_pct, "%", cache_color(&app.history.cache_pct), |v| format!("{v:.2}"));
    render_card(frame, cols[3], app, "latency p95 (est.)", &app.history.p95, "ms", p95_color(&app.history.p95), |v| format!("{v:.0}"));
}

fn conn_color(hist: &VecDeque<f64>, max_conn: i32) -> Color {
    if max_conn <= 0 {
        return theme::TEXT;
    }
    let ratio = hist.back().copied().unwrap_or(0.0) / max_conn as f64;
    if ratio > 0.85 { theme::BAD } else if ratio > 0.7 { theme::WARN } else { theme::TEXT }
}

fn cache_color(hist: &VecDeque<f64>) -> Color {
    let last = hist.back().copied().unwrap_or(100.0);
    if last < 98.0 { theme::WARN } else { theme::OK }
}

fn p95_color(hist: &VecDeque<f64>) -> Color {
    let last = hist.back().copied().unwrap_or(0.0);
    if last > 60.0 { theme::BAD } else if last > 30.0 { theme::WARN } else { theme::TEXT }
}

#[allow(clippy::too_many_arguments)]
fn render_card(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    label: &str,
    hist: &VecDeque<f64>,
    unit: &str,
    color: Color,
    fmt: impl Fn(f64) -> String,
) {
    let values: Vec<f64> = hist.iter().copied().collect();
    let last = values.last().copied().unwrap_or(0.0);
    let n_ago_idx = values.len().saturating_sub(6);
    let n_ago = values.get(n_ago_idx).copied().unwrap_or(last);
    let (arrow, pct) = charts::delta_arrow(last, n_ago, app.ascii);
    let spark = charts::sparkline(&values, 34, app.ascii);

    let lines = vec![
        Line::from(vec![
            Span::styled(label.to_string(), Style::default().fg(theme::TEXT_DIMMER)),
            Span::raw("  "),
            Span::styled(format!("{arrow}{pct:.1}%"), Style::default().fg(theme::TEXT_DIMMER)),
        ]),
        Line::from(vec![
            Span::styled(fmt(last), Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(unit.to_string(), Style::default().fg(theme::TEXT_DIMMER)),
        ]),
        Line::styled(spark, Style::default().fg(color)),
    ];
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme::BORDER)).style(Style::default().bg(theme::PANEL_BG));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_throughput(frame: &mut Frame, area: Rect, app: &App) {
    let hist: Vec<f64> = app.history.tps.iter().copied().collect();
    let width = area.width.saturating_sub(9).max(1) as usize;
    let height = area.height.saturating_sub(2).max(1) as usize;
    let (rows, hi) = charts::area_chart(&hist, height, width, app.ascii);

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let label = if i == 0 {
                format!("{hi:>6.0} ")
            } else if i == rows.len() / 2 {
                format!("{:>6.0} ", hi / 2.0)
            } else {
                "       ".to_string()
            };
            Line::from(vec![
                Span::styled(label, Style::default().fg(theme::TEXT_DIMMEST)),
                Span::styled(row.clone(), Style::default().fg(theme::OK)),
            ])
        })
        .collect();

    let rollback = app.rollback_pct.map(|p| format!(" · rollback {p:.1}%")).unwrap_or_default();
    let title = format!("Throughput · commits/s{rollback}");
    let block = theme::block(title);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_wait_events(frame: &mut Frame, area: Rect, app: &App) {
    let total: u64 = app.wait_event_counts.values().sum();
    let mut entries: Vec<(&String, &u64)> = app.wait_event_counts.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1));
    entries.truncate(6);

    let lines: Vec<Line> = if total == 0 {
        vec![Line::from("no samples yet")]
    } else {
        entries
            .iter()
            .map(|(name, count)| {
                let pct = **count as f64 / total as f64 * 100.0;
                let color = if pct > 25.0 { theme::BAD } else if pct > 10.0 { theme::WARN } else { theme::TEXT_DIM };
                Line::from(vec![
                    Span::styled(format!("{name:<22}"), Style::default().fg(theme::TEXT_DIM)),
                    Span::styled(charts::bar(pct, 20, app.ascii), Style::default().fg(color)),
                    Span::raw(format!(" {pct:.0}%")),
                ])
            })
            .collect()
    };

    let block = theme::block("Where Time Goes · wait events (sampled)");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_top_statements(frame: &mut Frame, area: Rect, app: &App) {
    let Some((StatementsData::Available(stmt_rows), _)) = &app.statements else {
        let block = theme::block("Slowest Statements");
        frame.render_widget(Paragraph::new("pg_stat_statements not loaded — see Queries tab").block(block), area);
        return;
    };

    let mut sorted: Vec<_> = stmt_rows.iter().collect();
    sorted.sort_by(|a, b| b.total_exec_time_ms.total_cmp(&a.total_exec_time_ms));

    let header = Row::new(vec!["mean ms", "calls", "query"]).style(Style::default().fg(theme::TEXT_DIMMEST));
    let rows = sorted.iter().take(5).map(|r| {
        let color = if r.mean_exec_time_ms > 300.0 { theme::BAD } else if r.mean_exec_time_ms > 80.0 { theme::WARN } else { theme::TEXT };
        Row::new(vec![
            Cell::from(format!("{:.1}", r.mean_exec_time_ms)).style(Style::default().fg(color)),
            Cell::from(r.calls.to_string()),
            Cell::from(r.query.clone()),
        ])
    });

    let widths = [Constraint::Length(9), Constraint::Length(8), Constraint::Min(10)];
    let table = Table::new(rows, widths).header(header).block(theme::block("Slowest Statements  (press 2 for full list)"));
    frame.render_widget(table, area);
}

fn draw_alerts(frame: &mut Frame, area: Rect, app: &App) {
    let inputs = build_inputs(app);
    let alerts = diagnosis::alerts(&inputs);

    let lines: Vec<Line> = if alerts.is_empty() {
        vec![Line::from("nothing needs attention right now")]
    } else {
        alerts
            .iter()
            .flat_map(|a| {
                let (mark, color) = match a.severity {
                    Severity::Bad => ("!", theme::BAD),
                    Severity::Warn => ("·", theme::WARN),
                };
                [
                    Line::from(vec![
                        Span::styled(format!("{mark} "), Style::default().fg(color)),
                        Span::styled(a.text.clone(), Style::default().fg(theme::TEXT_DIM)),
                    ]),
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(a.hint.clone(), Style::default().fg(theme::TEXT_DIMMEST)),
                    ]),
                ]
            })
            .collect()
    };

    let block = theme::block("Needs Attention");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
