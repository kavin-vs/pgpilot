use ratatui::{
    layout::{Constraint, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::ui::{charts, theme, widgets};

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.triggers.is_none() {
        widgets::loading_or_error(frame, area, app, "triggers", "Triggers");
        return;
    }
    let rows = app.triggers.as_ref().unwrap();
    if rows.is_empty() {
        let widget = Paragraph::new("no user-defined triggers in this database")
            .style(Style::default().fg(theme::TEXT_DIM))
            .block(theme::block("Triggers"));
        frame.render_widget(widget, area);
        return;
    }

    let header = Row::new(vec!["", "schema", "table", "trigger", "function", "state"])
        .style(Style::default().fg(theme::TEXT_DIMMEST));
    let selected = app.triggers_state.selected();
    let glyphs = charts::glyphs(app.ascii);

    let table_rows = rows.iter().enumerate().map(|(i, r)| {
        let cursor = if Some(i) == selected { glyphs.cursor } else { ' ' };
        let row_bg = if Some(i) == selected { theme::ROW_SELECTED_BG } else { theme::BG };
        let (state_text, state_color) =
            if r.enabled { ("enabled", theme::OK) } else { ("disabled", theme::TEXT_DIMMER) };

        Row::new(vec![
            Cell::from(cursor.to_string()).style(Style::default().fg(theme::OK)),
            Cell::from(r.schema_name.clone()),
            Cell::from(r.table_name.clone()),
            Cell::from(r.trigger_name.clone()),
            Cell::from(r.function_name.clone()),
            Cell::from(state_text).style(Style::default().fg(state_color)),
        ])
        .style(Style::default().bg(row_bg))
    });

    let widths = [
        Constraint::Length(2),
        Constraint::Percentage(14),
        Constraint::Percentage(18),
        Constraint::Percentage(24),
        Constraint::Percentage(24),
        Constraint::Percentage(12),
    ];

    let table = Table::new(table_rows, widths)
        .header(header)
        .block(theme::block("Triggers  (enter: view function source)"));

    frame.render_stateful_widget(table, area, &mut app.triggers_state);
}

/// Full-screen overlay showing the selected trigger's DDL + its function's
/// full body — same `Clear`+bordered-`Paragraph`+`Wrap` shape as
/// `widgets::draw_error_detail`. Opened with `enter` (only reachable with a
/// row selected, see `main.rs::handle_key`), closed with `enter`/`esc`/`q`.
pub fn draw_detail_popup(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(ratatui::widgets::Clear, area);

    let Some(row) = app.selected_trigger() else {
        return;
    };

    let mut lines: Vec<Line> = vec![Line::from(row.trigger_def.clone()), Line::from("")];
    lines.extend(row.function_def.lines().map(|l| Line::from(l.to_string())));

    let block = Block::default()
        .title(format!(
            "{}.{} on {} — function {}  (enter / esc / q to close)",
            row.schema_name, row.trigger_name, row.table_name, row.function_name
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_DETAIL))
        .style(Style::default().bg(theme::PANEL_BG));

    let widget = Paragraph::new(lines).block(block).wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(widget, area);
}
