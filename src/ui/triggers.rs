use ratatui::{
    layout::{Constraint, Rect},
    style::Style,
    text::Line,
    widgets::{Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::ui::{charts, theme, widgets};

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.triggers.is_none() {
        app.detail_pane_rect = None;
        app.table_pane_rect = None;
        widgets::loading_or_error(frame, area, app, "triggers", "Triggers");
        return;
    }
    if app.triggers.as_ref().unwrap().is_empty() {
        app.detail_pane_rect = None;
        app.table_pane_rect = None;
        let widget = Paragraph::new("no user-defined triggers in this database")
            .style(Style::default().fg(theme::TEXT_DIM))
            .block(theme::block("Triggers"));
        frame.render_widget(widget, area);
        return;
    }

    // No inline bottom pane (unlike Queries/Activity) — table takes the
    // whole tab, selected trigger's DDL/function body only shows in the
    // full-screen popup below (`enter` or click), scrollable there.
    app.detail_pane_rect = None;
    app.table_pane_rect = Some(area);
    draw_table(frame, area, app);
}

/// Full-screen view of the selected trigger's DDL/function body — opened by
/// `enter` or, while it existed, clicking the old inline pane
/// (`App::detail_popup_open`); see `queries::draw_detail_popup` (same shape,
/// mirrored per-tab since each tab's `draw_detail` builds different
/// content).
pub(crate) fn draw_detail_popup(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(ratatui::widgets::Clear, area);
    draw_detail(frame, area, app);
}

fn draw_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let rows = app.triggers.as_ref().unwrap();

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

    let table = Table::new(table_rows, widths).header(header).block(theme::block("Triggers"));

    frame.render_stateful_widget(table, area, &mut app.triggers_state);
}

/// Mirrors `queries.rs`'s `draw_detail` — an always-visible pane for
/// whichever row is highlighted, not an `enter`-triggered popup. Long
/// function bodies scroll via `PageUp`/`PageDown` (`widgets::draw_scrollable`)
/// rather than being clipped by the pane's height.
fn draw_detail(frame: &mut Frame, area: Rect, app: &App) {
    let Some(rows) = &app.triggers else {
        return;
    };
    let selected = app.triggers_state.selected().unwrap_or(0);
    let Some(row) = rows.get(selected) else {
        return;
    };

    let mut lines: Vec<Line> = vec![
        Line::from(format!("{}.{} on {} — function {}", row.schema_name, row.trigger_name, row.table_name, row.function_name)),
        Line::from(""),
        Line::from(row.trigger_def.clone()),
        Line::from(""),
    ];
    lines.extend(row.function_def.lines().map(|l| Line::from(l.to_string())));

    widgets::draw_scrollable(frame, area, "Selected Trigger", lines, app.detail_scroll);
}
