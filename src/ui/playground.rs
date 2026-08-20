use ratatui::{
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::App;
use crate::db::playground::StatementResult;
use crate::ui::{theme, widgets};

/// A psql-style REPL: a scrollable transcript of every command run this
/// session (region A) with the live input line(s) pinned below it (region
/// B, sized to its own content) — no separate "SQL"/"Output" boxes, no
/// F5-to-run-only flow. See CLAUDE.md's v14 note.
///
/// Both regions render inside one shared outer `Block` (rather than the
/// transcript drawing its own nested border via `widgets::draw_scrollable`)
/// so the tab reads as one continuous scrollback+prompt view with no seam
/// between output and input — "same like psql", not two separately-bordered
/// boxes stacked on top of each other.
pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    if let Some(err) = &app.playground_conn_error {
        widgets::error_block(frame, area, "Playground", err);
        return;
    }

    let prompt = app.current_dbname.as_deref().unwrap_or("pgpilot");

    // The live input area is sized to its own content (capped so a huge
    // paste can't swallow the whole screen), not a fixed split — unlike the
    // old editor-box/output-box layout, most of the time it's just one line.
    let input_lines = app.playground_editor.lines().len() as u16;
    let warn_line = u16::from(app.playground_confirm_pending);
    let input_height = (input_lines + warn_line).clamp(1, 8);

    // Reserve the outer block's own top/bottom border (2 rows) up front so
    // the transcript's wrap width/height matches what `Block::inner` will
    // actually hand back below.
    let content_height = area.height.saturating_sub(2);
    let inner_width = area.width.saturating_sub(2);
    let transcript_height = content_height.saturating_sub(input_height).max(3);

    let lines = transcript_lines(app, prompt);
    let (paragraph, clamped, total, max_scroll) =
        widgets::scrollable_paragraph(lines, inner_width, transcript_height, app.detail_scroll);
    app.playground_max_scroll = max_scroll;

    let title = if total > transcript_height {
        format!("Playground  [{}-{}/{}]  PgUp/PgDn", clamped + 1, (clamped + transcript_height).min(total), total)
    } else {
        "Playground".to_string()
    };
    let outer = theme::block(title);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(transcript_height), Constraint::Length(input_height)])
        .split(inner);

    frame.render_widget(paragraph, rows[0]);
    draw_input(frame, rows[1], app, prompt);
}

/// The first line of a statement gets `dbname=> `; continuation lines get a
/// plain `-> ` with no repeated database name — less visual noise than
/// real psql's own `dbname-> ` (a deliberate divergence from strict psql-
/// mimicry, a direct user preference).
fn prompt_marker(prompt: &str, i: usize) -> String {
    if i == 0 {
        format!("{prompt}=> ")
    } else {
        "-> ".to_string()
    }
}

/// One echoed prompt line per line of `sql` — see `prompt_marker`.
fn push_echo(lines: &mut Vec<Line<'static>>, prompt: &str, sql: &str) {
    let prompt_style = Style::default().fg(theme::TEXT_DIM).add_modifier(Modifier::BOLD);
    for (i, line) in sql.lines().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(prompt_marker(prompt, i), prompt_style),
            Span::raw(line.to_string()),
        ]));
    }
}

/// Builds the full scrollable transcript: every past command (echoed with
/// prompt lines) followed by its result/error/still-running spinner, oldest
/// first.
fn transcript_lines(app: &App, prompt: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for entry in &app.playground_transcript {
        push_echo(&mut lines, prompt, &entry.sql);
        match &entry.result {
            None => lines.push(Line::styled(
                format!("{} running…", widgets::spinner(app.spinner_frame)),
                Style::default().fg(theme::TEXT_DIM),
            )),
            Some(Ok(results)) => lines.extend(build_result_lines(results, entry.has_more, entry.fetching_more)),
            Some(Err(message)) => {
                lines.extend(message.lines().map(|l| Line::styled(l.to_string(), Style::default().fg(theme::BAD))))
            }
        }
        lines.push(Line::from(""));
    }
    lines
}

/// The live input line(s) plus a real terminal cursor — kept unwrapped
/// (unlike the transcript above) so cursor column math stays exact; a long
/// line simply runs off the visible width, same disclosed ceiling the old
/// editor pane had.
fn draw_input(frame: &mut Frame, area: Rect, app: &App, prompt: &str) {
    let editor = &app.playground_editor;
    let prompt_style = Style::default().fg(theme::TEXT_DIM).add_modifier(Modifier::BOLD);
    // The cursor's row determines which prompt (and thus which width) it
    // sits after — the first row's `dbname=> ` is wider than a continuation
    // row's plain `-> `.
    let prefix_width = prompt_marker(prompt, editor.cursor_row()).chars().count();

    let mut lines: Vec<Line> = editor
        .lines()
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut spans = vec![Span::styled(prompt_marker(prompt, i), prompt_style)];
            if editor.is_empty() && app.playground_transcript.is_empty() {
                spans.push(Span::styled(
                    "-- enter: run  \u{2191}/\u{2193}: history  ctrl-c: cancel",
                    Style::default().fg(theme::TEXT_DIMMEST),
                ));
            } else {
                spans.push(Span::raw(l.clone()));
            }
            Line::from(spans)
        })
        .collect();

    if app.playground_confirm_pending {
        lines.push(Line::styled(
            "non-SELECT statement — enter again to confirm, esc to cancel",
            Style::default().fg(theme::WARN),
        ));
    }

    frame.render_widget(Paragraph::new(lines), area);

    let x = area.x + prefix_width as u16 + editor.cursor_col() as u16;
    let y = area.y + editor.cursor_row() as u16;
    // No horizontal scroll for a long single line (known ceiling, same class
    // as the transcript's no-column-resize note below) — just skip painting
    // the cursor rather than let it land outside the area.
    if x < area.x + area.width && y < area.y + area.height {
        frame.set_cursor_position(Position { x, y });
    }
}

/// One psql-style ASCII table per statement: `|`-separated columns, a
/// dashes-and-`+` underline, numeric columns right-aligned (everything else
/// left), and a trailing `(N row)`/`(N rows)` line — matching real psql's
/// own aligned-table output format. No real column-resize/horizontal-scroll
/// for very wide result sets, a disclosed ceiling; a `── Statement N ──`
/// separator only appears once there's more than one, keeping the common
/// single-statement case clean.
fn build_result_lines(results: &[StatementResult], has_more: bool, fetching_more: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let multi = results.len() > 1;

    for (i, result) in results.iter().enumerate() {
        if multi {
            if i > 0 {
                lines.push(Line::from(""));
            }
            lines.push(Line::styled(format!("── Statement {} ──", i + 1), Style::default().fg(theme::TEXT_DIM)));
        }
        match result {
            StatementResult::Command { rows_affected } => {
                lines.push(Line::styled(
                    format!("{rows_affected} row(s) affected"),
                    Style::default().fg(theme::TEXT_DIM),
                ));
            }
            StatementResult::Rows { columns, rows } => {
                let columns: Vec<String> = columns.iter().map(|c| truncate_cell(c)).collect();
                let cell_rows: Vec<Vec<String>> = rows.iter().map(|r| r.iter().map(cell_text).collect()).collect();
                let widths = column_widths(&columns, &cell_rows);
                let right_align: Vec<bool> = (0..columns.len()).map(|ci| column_is_numeric(ci, rows)).collect();

                lines.push(Line::styled(
                    psql_row(&columns, &widths, &right_align),
                    Style::default().fg(theme::TEXT_DIMMEST).add_modifier(Modifier::BOLD),
                ));
                lines.push(Line::styled(psql_separator(&widths), Style::default().fg(theme::TEXT_DIMMEST)));
                for row in &cell_rows {
                    lines.push(Line::from(psql_row(row, &widths, &right_align)));
                }

                let n = rows.len();
                let count = format!("({n} row{})", if n == 1 { "" } else { "s" });
                let suffix = if fetching_more {
                    " — loading more…"
                } else if has_more {
                    " — PageDown for more"
                } else {
                    ""
                };
                lines.push(Line::styled(format!("{count}{suffix}"), Style::default().fg(theme::TEXT_DIM)));
            }
        }
    }
    lines
}

fn cell_text(v: &Option<String>) -> String {
    truncate_cell(&v.clone().unwrap_or_else(|| "NULL".to_string()))
}

/// Caps a cell's *displayed* width — not what's fetched — at
/// `CELL_MAX_CHARS` characters (char-safe: never byte-slices mid-UTF8-char).
/// This is the actual fix for the "Formatting argument out of range" panic:
/// a dynamic format width (`{c:>w$}`/`{c:<w$}`) is a `u16` internally, so an
/// unbounded cell (a multi-MB text/jsonb/bytea-hex value) could overflow it;
/// this bounds every width to `CELL_MAX_CHARS + 1`, far below that ceiling.
const CELL_MAX_CHARS: usize = 200;

fn truncate_cell(s: &str) -> String {
    if s.chars().count() > CELL_MAX_CHARS {
        let mut t: String = s.chars().take(CELL_MAX_CHARS).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

fn column_widths(columns: &[String], rows: &[Vec<String>]) -> Vec<usize> {
    columns
        .iter()
        .enumerate()
        .map(|(ci, c)| {
            rows.iter()
                .map(|r| r.get(ci).map(|s| s.chars().count()).unwrap_or(0))
                .chain(std::iter::once(c.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect()
}

/// Real psql right-aligns a column based on its actual SQL type (numeric
/// types), which the text-only `simple_query` protocol this app uses (see
/// `db::playground::run`'s doc) doesn't expose. ponytail: sniffs the
/// fetched *values* instead — right-aligned only if every non-`NULL` cell in
/// the column parses as a number. Good enough for the common case; an
/// all-text column that happens to hold only digit-like strings would
/// false-positive, and an all-`NULL` column (no value to sniff) falls back
/// to left — real psql would still know the type and could differ here.
fn column_is_numeric(ci: usize, rows: &[Vec<Option<String>>]) -> bool {
    let mut saw_value = false;
    for r in rows {
        if let Some(Some(v)) = r.get(ci) {
            saw_value = true;
            if v.trim().parse::<f64>().is_err() {
                return false;
            }
        }
    }
    saw_value
}

fn psql_row(cells: &[String], widths: &[usize], right_align: &[bool]) -> String {
    cells
        .iter()
        .zip(widths)
        .zip(right_align)
        .map(|((c, &w), &right)| if right { format!(" {c:>w$} ") } else { format!(" {c:<w$} ") })
        .collect::<Vec<_>>()
        .join("|")
}

fn psql_separator(widths: &[usize]) -> String {
    widths.iter().map(|w| "-".repeat(w + 2)).collect::<Vec<_>>().join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psql_row_matches_real_psql_spacing() {
        // Verified byte-for-byte against a real `psql` session:
        //  id | name
        // ----+-------
        //   1 | Alice
        let widths = vec![2, 5];
        let right_align = vec![true, false];
        assert_eq!(psql_row(&["id".to_string(), "name".to_string()], &widths, &right_align), " id | name  ");
        assert_eq!(psql_row(&["1".to_string(), "Alice".to_string()], &widths, &right_align), "  1 | Alice ");
        assert_eq!(psql_separator(&widths), "----+-------");
    }

    #[test]
    fn column_is_numeric_true_only_when_every_non_null_value_parses() {
        let numeric = vec![vec![Some("1".to_string())], vec![Some("-2.5".to_string())], vec![None]];
        assert!(column_is_numeric(0, &numeric));

        let mixed = vec![vec![Some("1".to_string())], vec![Some("abc".to_string())]];
        assert!(!column_is_numeric(0, &mixed));

        let all_null = vec![vec![None], vec![None]];
        assert!(!column_is_numeric(0, &all_null));
    }

    #[test]
    fn truncate_cell_leaves_short_strings_unchanged() {
        assert_eq!(truncate_cell("hello"), "hello");
    }

    #[test]
    fn truncate_cell_caps_and_appends_ellipsis() {
        let long = "a".repeat(CELL_MAX_CHARS + 50);
        let truncated = truncate_cell(&long);
        assert_eq!(truncated.chars().count(), CELL_MAX_CHARS + 1); // +1 for '…'
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_cell_is_char_safe_on_multi_byte_text() {
        let long = "é".repeat(CELL_MAX_CHARS + 50);
        let truncated = truncate_cell(&long);
        assert_eq!(truncated.chars().count(), CELL_MAX_CHARS + 1);
    }

    /// Direct regression test for the reported "Formatting argument out of
    /// range" panic: a single oversized cell must not blow past the dynamic
    /// width's `u16` limit.
    #[test]
    fn oversized_cell_does_not_panic_psql_row() {
        let huge = Some("x".repeat(100_000));
        let cells: Vec<String> = vec![cell_text(&huge)];
        let widths = vec![cells[0].chars().count()];
        assert!(widths[0] < u16::MAX as usize);
        psql_row(&cells, &widths, &[false]); // must not panic
    }
}
