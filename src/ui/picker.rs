//! Full-screen database picker, opened with `d`. Replaces the old small
//! popup overlay — richer columns (owner/size/sessions/tps/cache-hit/state),
//! rendered full-screen rather than centered since there's a lot to show.

use std::collections::HashMap;

use ratatui::{
    layout::Constraint,
    style::Style,
    widgets::{Cell, Clear, Row, Table},
    Frame,
};

use crate::app::App;
use crate::format::human_bytes;
use crate::ui::{charts, theme, widgets};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let title = "Switch Database  (j/k select, enter confirm, esc/q/d cancel)";

    let Some((databases, _)) = &app.databases else {
        widgets::loading_or_error(frame, area, app, "databases", "databases");
        return;
    };

    let tps_by_name = tps_deltas(app);
    let selected = app.db_popup.unwrap_or(0);
    let glyphs = charts::glyphs(app.ascii);

    let header = Row::new(vec!["", "database", "owner", "size", "sessions", "tps", "cache hit", "state"])
        .style(Style::default().fg(theme::TEXT_DIMMEST));

    let rows = databases.iter().enumerate().map(|(i, d)| {
        let is_current = Some(d.name.as_str()) == app.current_dbname.as_deref();
        let cursor = if i == selected { glyphs.cursor } else { ' ' };
        let marker = if is_current { "* " } else { "" };
        let name_color = if d.unswitchable_reason().is_some() {
            theme::TEXT_DIMMEST
        } else if is_current {
            theme::OK
        } else {
            theme::TEXT_BRIGHT
        };
        let cache = d
            .cache_hit_pct
            .map(|p| format!("{p:.1}%"))
            .unwrap_or_else(|| "—".to_string());
        let cache_color = match d.cache_hit_pct {
            Some(p) if p < 90.0 => theme::WARN,
            Some(_) => theme::OK,
            None => theme::TEXT_DIMMER,
        };
        let tps = tps_by_name
            .get(&d.name)
            .map(|t| format!("{t:.0}"))
            .unwrap_or_else(|| "—".to_string());
        let state = if d.is_template {
            "template · not connectable"
        } else if d.is_reserved() {
            "reserved · not connectable"
        } else if is_current {
            "current"
        } else {
            ""
        };

        let row_bg = if i == selected { theme::ROW_SELECTED_BG } else { theme::BG };
        Row::new(vec![
            Cell::from(cursor.to_string()).style(Style::default().fg(theme::OK)),
            Cell::from(format!("{marker}{}", d.name)).style(Style::default().fg(name_color)),
            Cell::from(d.owner.clone()).style(Style::default().fg(theme::TEXT_DIM)),
            Cell::from(human_bytes(d.size_bytes)).style(Style::default().fg(theme::TEXT_DIM)),
            Cell::from(d.sessions.to_string()).style(Style::default().fg(theme::TEXT_DIM)),
            Cell::from(tps).style(Style::default().fg(theme::TEXT_DIM)),
            Cell::from(cache).style(Style::default().fg(cache_color)),
            Cell::from(state).style(Style::default().fg(theme::TEXT_DIMMER)),
        ])
        .style(Style::default().bg(row_bg))
    });

    let widths = [
        Constraint::Length(2),
        Constraint::Percentage(28),
        Constraint::Percentage(12),
        Constraint::Percentage(10),
        Constraint::Percentage(10),
        Constraint::Percentage(8),
        Constraint::Percentage(10),
        Constraint::Percentage(20),
    ];

    let table = Table::new(rows, widths).header(header).block(theme::block(title));

    frame.render_widget(table, area);
}

/// Per-database tps from the delta between the two most recent database-list
/// polls (`app.databases`/`app.databases_prev`) — empty until a second poll
/// has landed.
fn tps_deltas(app: &App) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    let (Some((cur, cur_at)), Some((prev, prev_at))) = (&app.databases, &app.databases_prev) else {
        return out;
    };
    let dt = cur_at.duration_since(*prev_at).as_secs_f64();
    if dt <= 0.0 {
        return out;
    }
    for d in cur {
        if let Some(p) = prev.iter().find(|p| p.name == d.name) {
            let delta = (d.xact_commit + d.xact_rollback) - (p.xact_commit + p.xact_rollback);
            out.insert(d.name.clone(), (delta as f64 / dt).max(0.0));
        }
    }
    out
}
