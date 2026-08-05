use std::collections::{HashMap, VecDeque};

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
use crate::format::human_bytes;
use crate::ui::{charts, theme, widgets};

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    if app.connections.is_none() && app.cache_overall.is_none() && app.activity.is_none() {
        widgets::loading(frame, area, "Overview", app.spinner_frame);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(0)])
        .split(area);

    draw_cards(frame, rows[0], app);

    let rest = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30), // top statements / wait events
            Constraint::Percentage(42), // buffer cache / per-database / coldest
            Constraint::Percentage(28), // checkpoints & buffers / replication
        ])
        .split(rows[1]);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rest[0]);
    draw_top_statements(frame, top[0], app);
    draw_wait_events(frame, top[1], app);

    let cache = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rest[1]);
    draw_cache_detail(frame, cache[0], app);
    draw_coldest(frame, cache[1], app);

    let checkpoints = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rest[2]);
    draw_checkpoints(frame, checkpoints[0], app);
    draw_replication(frame, checkpoints[1], app);
}

fn fmt_pct(pct: Option<f64>) -> String {
    match pct {
        Some(p) => format!("{p:.2}%"),
        None => "n/a".to_string(),
    }
}

pub(crate) fn build_inputs(app: &App) -> DiagnosisInputs<'_> {
    DiagnosisInputs {
        tables: app.tables.as_ref().map(|(d, _)| d),
        indexes: app.indexes.as_deref(),
        unindexed_fks: app.unindexed_fks.as_deref().unwrap_or(&[]),
        activity: app.activity.as_ref(),
        statements: app.statements.as_ref().map(|(d, _)| d),
        cache: app.cache_overall.as_ref().map(|(d, _)| d),
        connections: app.connections.as_ref(),
        cache_checkpoints: app.cache_checkpoints.as_ref(),
        replication: app.cache_replication.as_deref(),
        temp_bytes_per_sec: app.temp_bytes_per_sec,
    }
}

fn severity_color(s: Severity) -> Color {
    match s {
        Severity::Warn => theme::WARN,
        Severity::Bad => theme::BAD,
    }
}

fn draw_cards(frame: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(40), Constraint::Percentage(20)])
        .split(area);

    let max_conn = app.connections.as_ref().map(|c| c.max_connections).unwrap_or(0);

    render_card(frame, cols[0], app, "transactions", &app.history.tps, "/s", theme::TEXT, |v| format!("{v:.0}"));
    render_card(frame, cols[1], app, "latency p95 (est.)", &app.history.p95, "ms", p95_color(&app.history.p95), |v| format!("{v:.0}"));

    let conn_last = app.history.conn.back().copied().unwrap_or(0.0);
    draw_meter(frame, cols[2], "connections", conn_last, max_conn as f64, &format!(" /{max_conn}"), conn_color(&app.history.conn, max_conn), app.ascii);
}

#[allow(clippy::too_many_arguments)]
fn draw_meter(frame: &mut Frame, area: Rect, label: &str, value: f64, max: f64, unit: &str, color: Color, ascii: bool) {
    let pct = if max > 0.0 { (value / max * 100.0).clamp(0.0, 100.0) } else { 0.0 };
    let line = Line::from(vec![
        Span::styled(format!("{label:<11}"), Style::default().fg(theme::TEXT_DIMMER)),
        Span::styled(charts::bar(pct, 10, ascii), Style::default().fg(color)),
        Span::raw(" "),
        Span::styled(format!("{value:.0}{unit}"), Style::default().fg(color)),
    ]);
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme::BORDER)).style(Style::default().bg(theme::PANEL_BG));
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn conn_color(hist: &VecDeque<f64>, max_conn: i32) -> Color {
    if max_conn <= 0 {
        return theme::TEXT;
    }
    let ratio = hist.back().copied().unwrap_or(0.0) / max_conn as f64;
    if ratio > 0.85 { theme::BAD } else if ratio > 0.7 { theme::WARN } else { theme::TEXT }
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

const IDLE_WAIT_KEY: &str = "idle (no active query)";

// Percent of *every* poll in the window, not just the ones that caught
// something active — dividing by active-only samples is what previously
// made "CPU / running" read as ~100% even when RDS reported the database
// as nearly idle. A poll that caught zero active backends contributes an
// empty sample (see `App::wait_event_samples`), which becomes an explicit
// `IDLE_WAIT_KEY` bucket here so that denominator is visible, not implicit.
// Percentages across buckets can sum past 100% when multiple backends were
// concurrently active in the same poll — that's an "average concurrent
// sessions" style number (same idea as RDS Performance Insights' DB Load),
// not a bug.
fn draw_wait_events(frame: &mut Frame, area: Rect, app: &App) {
    let total_polls = app.wait_event_samples.len();

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for sample in &app.wait_event_samples {
        if sample.is_empty() {
            *counts.entry(IDLE_WAIT_KEY).or_insert(0) += 1;
        } else {
            for key in sample {
                *counts.entry(key.as_str()).or_insert(0) += 1;
            }
        }
    }
    let mut entries: Vec<(&str, usize)> = counts.into_iter().collect();
    entries.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    entries.truncate(6);

    let lines: Vec<Line> = if total_polls == 0 {
        vec![Line::from("no samples yet")]
    } else {
        entries
            .iter()
            .map(|(name, count)| {
                let pct = *count as f64 / total_polls as f64 * 100.0;
                let color = if *name == IDLE_WAIT_KEY {
                    theme::TEXT_DIM
                } else if pct > 25.0 {
                    theme::BAD
                } else if pct > 10.0 {
                    theme::WARN
                } else {
                    theme::TEXT_DIM
                };
                Line::from(vec![
                    Span::styled(format!("{name:<22}"), Style::default().fg(theme::TEXT_DIM)),
                    Span::styled(charts::bar(pct.min(100.0), 20, app.ascii), Style::default().fg(color)),
                    Span::raw(format!(" {pct:.0}%")),
                ])
            })
            .collect()
    };

    let block = theme::block(format!("Where Time Goes · avg active by wait type (last {total_polls} polls)"));
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

fn draw_cache_detail(frame: &mut Frame, area: Rect, app: &App) {
    let Some((cache, _)) = &app.cache_overall else {
        widgets::loading_or_error(frame, area, app, "cache overview", "Buffer Cache");
        return;
    };

    let hit = cache.hit_ratio_pct;
    let color = match hit {
        Some(p) if p < 98.0 => theme::WARN,
        Some(_) => theme::OK,
        None => theme::TEXT_DIM,
    };
    let cache_hist: Vec<f64> = app.history.cache_pct.iter().copied().collect();
    let spark = charts::sparkline(&cache_hist, 72, app.ascii);

    let mut lines = vec![
        Line::from(vec![
            Span::raw("Buffer cache hit ratio: "),
            Span::styled(fmt_pct(hit), Style::default().fg(color)),
            Span::raw("  (target >= 99%)"),
        ]),
        Line::from(Span::styled(spark, Style::default().fg(color))),
        Line::from(""),
        Line::styled("cumulative since last stats reset:", Style::default().fg(theme::TEXT_DIMMEST)),
        Line::from(format!("blocks hit:  {}", cache.blks_hit)),
        Line::from(format!("blocks read: {}", cache.blks_read)),
        Line::from(format!("temp files:  {} ({} total)", cache.temp_files, human_bytes(cache.temp_bytes))),
    ];
    if let Some(rate) = app.temp_bytes_per_sec {
        let label = if rate > 0.0 { format!("temp spill rate: {}/s", human_bytes(rate as i64)) } else { "temp spill rate: 0 B/s".to_string() };
        lines.push(Line::styled(label, Style::default().fg(theme::TEXT_DIM)));
    }
    if let Some(pct) = app.rollback_pct {
        lines.push(Line::from(format!("rollback share of throughput: {pct:.1}%")));
    }

    let block = theme::block("Buffer Cache");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_coldest(frame: &mut Frame, area: Rect, app: &App) {
    let Some(coldest) = &app.cache_coldest else {
        widgets::loading_or_error(frame, area, app, "coldest relations", "Coldest Relations");
        return;
    };

    let header = Row::new(vec!["relation", "hit ratio", "hits/reads", "size"]).style(Style::default().fg(theme::TEXT_DIMMEST));
    let rows = coldest.iter().map(|r| {
        let color = match r.hit_ratio_pct {
            Some(p) if p < 85.0 => theme::BAD,
            Some(p) if p < 98.0 => theme::WARN,
            _ => theme::OK,
        };
        let bar = charts::bar(r.hit_ratio_pct.unwrap_or(0.0), 10, false);
        Row::new(vec![
            Cell::from(format!("{}.{}", r.schema_name, r.table_name)),
            Cell::from(format!("{bar} {}", fmt_pct(r.hit_ratio_pct))).style(Style::default().fg(color)),
            Cell::from(format!("{}/{}", r.heap_blks_hit, r.heap_blks_read)),
            Cell::from(human_bytes(r.size_bytes)),
        ])
    });

    let widths = [
        Constraint::Percentage(35),
        Constraint::Percentage(30),
        Constraint::Percentage(20),
        Constraint::Percentage(15),
    ];
    let table = Table::new(rows, widths).header(header).block(theme::block("Coldest Relations · lowest cache hit"));
    frame.render_widget(table, area);
}

fn draw_checkpoints(frame: &mut Frame, area: Rect, app: &App) {
    let Some(bg) = &app.cache_checkpoints else {
        widgets::loading_or_error(frame, area, app, "checkpoints & wal", "Checkpoints & Buffers");
        return;
    };

    let total = bg.checkpoints_timed + bg.checkpoints_req;
    let req_pct = if total > 0 { bg.checkpoints_req as f64 / total as f64 * 100.0 } else { 0.0 };
    let checkpoints_color = if bg.checkpoints_req > bg.checkpoints_timed { theme::WARN } else { theme::TEXT };
    let bar = charts::bar(req_pct, 20, app.ascii);

    let backend = bg.buffers_backend.map(|v| v.to_string()).unwrap_or_else(|| "n/a (PG17+)".to_string());
    let clean_color = if bg.maxwritten_clean > 0 { theme::WARN } else { theme::TEXT };

    let lines = vec![
        Line::from(vec![
            Span::styled("checkpoints  ", Style::default().fg(theme::TEXT_DIMMER)),
            Span::styled(bar, Style::default().fg(checkpoints_color)),
            Span::styled(format!("  {} timed / {} requested", bg.checkpoints_timed, bg.checkpoints_req), Style::default().fg(checkpoints_color)),
        ]),
        Line::from(""),
        Line::styled("buffers written", Style::default().fg(theme::TEXT_DIMMER)),
        Line::from(vec![
            Span::styled(format!("checkpoint: {}   ", bg.buffers_checkpoint), Style::default().fg(theme::TEXT)),
            Span::styled(format!("clean: {} (maxwritten {})", bg.buffers_clean, bg.maxwritten_clean), Style::default().fg(clean_color)),
        ]),
        Line::styled(format!("backend: {backend}   alloc: {}", bg.buffers_alloc), Style::default().fg(theme::TEXT_DIM)),
    ];

    frame.render_widget(Paragraph::new(lines).block(theme::block("Checkpoints & Buffers")), area);
}

fn draw_replication(frame: &mut Frame, area: Rect, app: &App) {
    let Some(replication) = &app.cache_replication else {
        widgets::loading_or_error(frame, area, app, "replication", "Replication");
        return;
    };

    let text = if replication.is_empty() {
        "no replicas".to_string()
    } else {
        replication
            .iter()
            .map(|r| {
                let bytes = r.lag_bytes.map(human_bytes).unwrap_or_else(|| "?".to_string());
                match r.replay_lag_secs {
                    Some(secs) => format!("{}: {} ({} behind)", r.application_name, bytes, crate::format::human_duration(secs)),
                    None => format!("{}: {}", r.application_name, bytes),
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let block = theme::block("Replication");
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(theme::TEXT)).block(block), area);
}

/// Full-screen diagnosis overlay — headline + ranked suspects + a short list
/// of fix hints (reusing `alerts()`'s `.hint` text rather than adding new
/// fields to `diagnosis.rs`). Opened with `g`, closed with `g`/`esc`/`q`.
/// Takes the already-computed `Diagnosis`/`Alert`s rather than `&App` since
/// `ui/mod.rs` computes both exactly once per frame.
pub fn draw_diagnosis_modal(frame: &mut Frame, diag: &diagnosis::Diagnosis, alerts: &[diagnosis::Alert]) {
    let area = frame.area();
    frame.render_widget(ratatui::widgets::Clear, area);

    let mut lines = vec![Line::styled(diag.headline.clone(), Style::default().fg(theme::TEXT_BRIGHT)), Line::raw("")];
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
    if diag.suspects.is_empty() {
        lines.push(Line::from("no issues detected"));
    }

    if !alerts.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Fixes", Style::default().fg(theme::TEXT_DIM)));
        for a in alerts.iter().take(8) {
            let color = severity_color(a.severity);
            lines.push(Line::from(vec![
                Span::styled("  · ", Style::default().fg(color)),
                Span::styled(a.text.clone(), Style::default().fg(theme::TEXT_DIM)),
                Span::raw(" — "),
                Span::styled(a.hint.clone(), Style::default().fg(theme::TEXT_DIMMEST)),
            ]));
        }
    }

    let block = Block::default()
        .title("Diagnose  (g / esc / q to close)")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_DETAIL))
        .style(Style::default().bg(theme::PANEL_BG));
    frame.render_widget(Paragraph::new(lines).block(block).wrap(ratatui::widgets::Wrap { trim: false }), area);
}
