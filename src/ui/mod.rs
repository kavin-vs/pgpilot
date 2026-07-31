pub mod activity;
pub mod charts;
pub mod overview;
pub mod picker;
pub mod queries;
pub mod tables_indexes;
pub mod theme;
pub mod triggers;
pub mod widgets;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::app::{App, PanelKind};
use crate::diagnosis;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    // Computed once per frame — the owned `Diagnosis`/`Vec<Alert>` results
    // outlive the `&App` borrow `build_inputs` takes, so they don't fight the
    // `&mut App` the tab draws below need for their own stateful widgets
    // (table selection, etc.).
    let (diag, alerts) = {
        let inputs = overview::build_inputs(app);
        (diagnosis::diagnose(&inputs), diagnosis::alerts(&inputs))
    };

    widgets::draw_header(frame, chunks[0], app);
    widgets::draw_tab_bar(frame, chunks[1], app.active, &diag);

    match app.active {
        PanelKind::Overview => overview::draw(frame, chunks[2], app),
        PanelKind::Queries => queries::draw(frame, chunks[2], app),
        PanelKind::Activity => activity::draw(frame, chunks[2], app),
        PanelKind::TablesIndexes => tables_indexes::draw(frame, chunks[2], app),
        PanelKind::Triggers => triggers::draw(frame, chunks[2], app),
    }

    widgets::draw_footer(frame, chunks[3], app);

    if app.db_popup.is_some() {
        picker::draw(frame, app);
    }

    if app.error_detail_open {
        widgets::draw_error_detail(frame, app);
    }

    if app.trigger_detail_open {
        triggers::draw_detail_popup(frame, app);
    }

    if app.activity_detail_open {
        activity::draw_detail_popup(frame, app);
    }

    if app.diagnosis_open {
        overview::draw_diagnosis_modal(frame, &diag, &alerts);
    }
}
