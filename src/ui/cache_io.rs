use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::format::human_bytes;
use crate::ui::{charts, theme, widgets};

fn fmt_pct(pct: Option<f64>) -> String {
    match pct {
        Some(p) => format!("{p:.2}%"),
        None => "n/a".to_string(),
    }
}

/// Every block here is backed by its own independently-fetched App field
/// (see `PanelSnapshot::source_label`) — one query failing shows an error
/// state on just that block, not a blank tab.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(33), Constraint::Percentage(33)])
        .split(rows[0]);
    draw_cache_detail(frame, top[0], app);
    draw_per_database(frame, top[1], app);
    draw_coldest(frame, top[2], app);

    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[1]);
    draw_checkpoints(frame, bottom[0], app);
    draw_replication(frame, bottom[1], app);
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
        Line::from(format!("blocks hit:  {}", cache.blks_hit)),
        Line::from(format!("blocks read: {}", cache.blks_read)),
        Line::from(format!("temp files:  {} ({})", cache.temp_files, human_bytes(cache.temp_bytes))),
    ];
    if let Some(pct) = app.rollback_pct {
        lines.push(Line::from(format!("rollback share of throughput: {pct:.1}%")));
    }

    let block = theme::block("Buffer Cache");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_per_database(frame: &mut Frame, area: Rect, app: &App) {
    let Some(per_database) = &app.cache_per_database else {
        widgets::loading_or_error(frame, area, app, "per-database cache", "Per Database");
        return;
    };

    let header = Row::new(vec!["database", "hit ratio"]).style(Style::default().fg(theme::TEXT_DIMMEST));
    let rows = per_database.iter().map(|db| {
        let color = match db.hit_ratio_pct {
            Some(p) if p < 90.0 => theme::WARN,
            Some(_) => theme::OK,
            None => theme::TEXT_DIM,
        };
        Row::new(vec![
            Cell::from(db.datname.clone()),
            Cell::from(fmt_pct(db.hit_ratio_pct)).style(Style::default().fg(color)),
        ])
    });
    let widths = [Constraint::Percentage(60), Constraint::Percentage(40)];
    let table = Table::new(rows, widths).header(header).block(theme::block("Per Database"));
    frame.render_widget(table, area);
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
        widgets::loading_or_error(frame, area, app, "checkpoints & wal", "Checkpoints & WAL");
        return;
    };

    let backend = bg
        .buffers_backend
        .map(|v| v.to_string())
        .unwrap_or_else(|| "n/a (PG17+)".to_string());
    let text = vec![Line::from(format!(
        "checkpoints: {} timed / {} requested   buffers: {} checkpoint, {} clean ({} maxwritten), {} backend, {} alloc",
        bg.checkpoints_timed, bg.checkpoints_req, bg.buffers_checkpoint, bg.buffers_clean, bg.maxwritten_clean, backend, bg.buffers_alloc,
    ))];

    let color = if bg.checkpoints_req > bg.checkpoints_timed { theme::WARN } else { theme::TEXT };

    let block = theme::block("Checkpoints & WAL");
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(color)).block(block), area);
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
            .map(|r| format!("{}: {}", r.application_name, r.lag_bytes.map(human_bytes).unwrap_or_else(|| "?".to_string())))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let block = theme::block("Replication");
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(theme::TEXT)).block(block), area);
}
