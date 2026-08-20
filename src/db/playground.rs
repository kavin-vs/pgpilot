use tokio::sync::mpsc;
use tokio_postgres::{Client, SimpleQueryMessage};

use super::tls::TlsMode;
use crate::cli::{build_conninfo, ConnParts};
use crate::event::AppEvent;

/// One statement's outcome from a `simple_query()` call — either a row set
/// (even an empty `SELECT` still gets `Rows` with zero rows, since it had a
/// `RowDescription`) or a bare row-affected count for a non-SELECT statement
/// (no command-tag text like psql's "UPDATE 3" — `simple_query` only gives a
/// number).
#[derive(Debug, PartialEq)]
pub enum StatementResult {
    Rows { columns: Vec<String>, rows: Vec<Vec<Option<String>>> },
    Command { rows_affected: u64 },
}

/// Commands sent to `playground_task` — mirrors `db::PollControl`'s shape,
/// scoped to just what the Playground connection needs.
pub enum PlaygroundControl {
    Run(String),
    SwitchDb(String),
    /// Requests the next page of a paginated result (see
    /// `is_single_paginatable_select`) — a no-op if nothing is currently
    /// paginated (defensive; shouldn't happen since the UI only sends this
    /// after itself observing `has_more`).
    FetchMore,
}

/// Rows fetched per page, and the trigger for treating a `Run` as
/// paginatable at all — matches DBeaver's own default fetch size.
const PLAYGROUND_PAGE_SIZE: i64 = 200;

/// Owned, testable mirror of `SimpleQueryMessage` — `SimpleQueryRow::new`/
/// `SimpleColumn::new` are `pub(crate)` to tokio-postgres, so a message
/// carrying real data can't be constructed outside that crate for a unit
/// test. `to_owned` is the trivial, untested boundary; `group` (below) is
/// the fully-tested pure logic.
enum Msg {
    Columns(Vec<String>),
    Row(Vec<Option<String>>),
    Done(u64),
}

fn to_owned(messages: Vec<SimpleQueryMessage>) -> Vec<Msg> {
    messages
        .into_iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::RowDescription(cols) => {
                Some(Msg::Columns(cols.iter().map(|c| c.name().to_string()).collect()))
            }
            SimpleQueryMessage::Row(row) => {
                let n = row.columns().len();
                Some(Msg::Row((0..n).map(|i| row.get(i).map(str::to_string)).collect()))
            }
            SimpleQueryMessage::CommandComplete(n) => Some(Msg::Done(n)),
            // `SimpleQueryMessage` is `#[non_exhaustive]` — a future variant
            // is a no-op here rather than a compile break.
            _ => None,
        })
        .collect()
}

/// Groups a flat message stream into one `StatementResult` per statement:
/// a `RowDescription` opens a row set, `Row`s accumulate into it, and the
/// next `CommandComplete` flushes whatever's pending (a `Rows` result if a
/// `RowDescription` preceded it, a bare `Command` otherwise) — exactly the
/// interleaving `Client::simple_query` produces for a multi-statement script.
fn group(messages: Vec<Msg>) -> Vec<StatementResult> {
    let mut results = Vec::new();
    let mut pending_columns: Option<Vec<String>> = None;
    let mut pending_rows: Vec<Vec<Option<String>>> = Vec::new();

    for msg in messages {
        match msg {
            Msg::Columns(cols) => pending_columns = Some(cols),
            Msg::Row(vals) => pending_rows.push(vals),
            Msg::Done(n) => {
                let result = match pending_columns.take() {
                    Some(columns) => StatementResult::Rows { columns, rows: std::mem::take(&mut pending_rows) },
                    None => StatementResult::Command { rows_affected: n },
                };
                results.push(result);
            }
        }
    }
    results
}

pub async fn run(client: &Client, sql: &str) -> Result<Vec<StatementResult>, tokio_postgres::Error> {
    Ok(group(to_owned(client.simple_query(sql).await?)))
}

/// ponytail: naive top-level `;` split — doesn't understand semicolons
/// inside string/dollar-quoted literals. Upgrade to a real tokenizer if that
/// bites; good enough for the confirm-guard heuristic below, which only
/// needs to classify each chunk, not execute it (execution goes through the
/// whole original text via `simple_query`, unaffected by this split).
pub fn split_statements(sql: &str) -> Vec<String> {
    sql.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn is_read_only_statement(stmt: &str) -> bool {
    let upper = stmt.trim_start().to_uppercase();
    ["SELECT", "WITH", "EXPLAIN", "SHOW", "TABLE"].iter().any(|p| upper.starts_with(p))
}

/// Whether `sql` contains at least one statement that isn't read-only-looking
/// — gates the Playground confirm guard (see `CLAUDE.md`'s v11 note).
pub fn needs_confirmation(sql: &str) -> bool {
    let statements = split_statements(sql);
    !statements.is_empty() && statements.iter().any(|s| !is_read_only_statement(s))
}

/// ponytail: `SELECT`-only, deliberately narrower than `is_read_only_statement`
/// above (which also allows `WITH`/`EXPLAIN`/`SHOW`/`TABLE`). A `WITH` query
/// can contain a data-modifying CTE (`WITH deleted AS (DELETE FROM foo
/// RETURNING *) SELECT * FROM deleted`), and since each page re-runs the
/// whole inner query from scratch (see `page_query`), paginating a `WITH`
/// would silently re-run that `DELETE` on every scroll-triggered fetch.
/// `EXPLAIN`/`SHOW`/`TABLE` aren't meaningful/valid as a `FROM (...)`
/// subquery source anyway. Excluded statements just keep the existing
/// full-buffer path (now width-safe, see `ui/playground.rs::truncate_cell`).
/// Upgrade path if `WITH` pagination is ever requested: scan the CTE body
/// for write keywords before allowing it.
fn is_paginatable_select(stmt: &str) -> bool {
    stmt.trim_start().to_uppercase().starts_with("SELECT")
}

/// `Some(trimmed sql)` iff `sql` is exactly one statement and it's a bare
/// `SELECT` (see `is_paginatable_select`) — anything else (multi-statement
/// scripts, DML/DDL, `WITH`) isn't paginated.
pub fn is_single_paginatable_select(sql: &str) -> Option<String> {
    match split_statements(sql).as_slice() {
        [one] if is_paginatable_select(one) => Some(one.clone()),
        _ => None,
    }
}

/// Wraps `base` (assumed a bare `SELECT`, from `is_single_paginatable_select`)
/// as an opaque subquery source — works uniformly without parsing/rewriting
/// its insides, and composes correctly with any `ORDER BY`/`LIMIT` `base`
/// already has (evaluated inside the subquery before this `LIMIT`/`OFFSET`
/// slices it). ponytail: stateless re-fetch, not a held server-side cursor —
/// simpler, and avoids holding a transaction open (which would hold back the
/// cluster-wide vacuum/xmin horizon for every other connection, not just
/// this one). Known ceiling: each page re-runs `base` from scratch (no
/// cross-page caching) and isn't snapshot-consistent under concurrent writes
/// between pages — fine for an ad-hoc console, not a live consistent grid.
fn page_query(base: &str, offset: i64) -> String {
    format!("SELECT * FROM ({base}) AS pgpilot_page LIMIT {PLAYGROUND_PAGE_SIZE} OFFSET {offset}")
}

/// `pg_backend_pid()` for the task's current `client` — used to target this
/// connection's own `pg_cancel_backend` request when the user hits Ctrl+C on
/// a running query (see `AppEvent::PlaygroundPid`). Best-effort: if this one
/// query fails (essentially never), Ctrl+C just silently has nothing to
/// target this session, no error surfaced for it.
async fn backend_pid(client: &Client) -> Option<i32> {
    client.query_one("SELECT pg_backend_pid()", &[]).await.ok()?.try_get(0).ok()
}

/// Background task owning the Playground's dedicated `Client` — separate
/// from `db::poll_task`'s so a slow/lock-held ad-hoc query never blocks the
/// live dashboard's own polling. Purely command-driven (no ticker): idle
/// until told to run a statement or follow a database switch.
pub async fn playground_task(
    mut client: Client,
    tls: TlsMode,
    conn_base: Option<ConnParts>,
    tx: mpsc::Sender<AppEvent>,
    mut control_rx: mpsc::Receiver<PlaygroundControl>,
) {
    if let Some(pid) = backend_pid(&client).await
        && tx.send(AppEvent::PlaygroundPid(pid)).await.is_err()
    {
        return;
    }

    // Local pagination state (base query text, next offset) for whatever
    // was last run, if it was paginatable — not `App` state, since `App`
    // only needs the UI-facing has-more/fetching flags, not the query text.
    let mut paginated: Option<(String, i64)> = None;

    while let Some(cmd) = control_rx.recv().await {
        match cmd {
            PlaygroundControl::Run(sql) => {
                paginated = None;
                let (sql_to_run, base_for_pagination) = match is_single_paginatable_select(&sql) {
                    Some(base) => (page_query(&base, 0), Some(base)),
                    None => (sql, None),
                };
                let result = run(&client, &sql_to_run).await.map_err(|e| crate::db::chained_message(&e));
                let has_more = match (&result, &base_for_pagination) {
                    (Ok(stmts), Some(base)) => {
                        let more = matches!(stmts.last(), Some(StatementResult::Rows { rows, .. }) if rows.len() as i64 == PLAYGROUND_PAGE_SIZE);
                        if more {
                            paginated = Some((base.clone(), PLAYGROUND_PAGE_SIZE));
                        }
                        more
                    }
                    _ => false,
                };
                if tx.send(AppEvent::PlaygroundResult { result, has_more }).await.is_err() {
                    return;
                }
            }
            PlaygroundControl::FetchMore => {
                let Some((base, offset)) = paginated.clone() else {
                    // Defensive: shouldn't happen in normal operation, but
                    // must still clear the UI's in-flight flag rather than
                    // silently no-op it stuck forever.
                    if tx
                        .send(AppEvent::PlaygroundMore { result: Err("no more pages to fetch".to_string()), has_more: false })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                };
                let result = run(&client, &page_query(&base, offset)).await.map_err(|e| crate::db::chained_message(&e));
                let (statement, has_more) = match result {
                    Ok(mut stmts) => {
                        let statement =
                            stmts.pop().unwrap_or(StatementResult::Rows { columns: vec![], rows: vec![] });
                        let more = matches!(&statement, StatementResult::Rows { rows, .. } if rows.len() as i64 == PLAYGROUND_PAGE_SIZE);
                        paginated = if more { Some((base, offset + PLAYGROUND_PAGE_SIZE)) } else { None };
                        (Ok(statement), more)
                    }
                    Err(e) => {
                        paginated = None;
                        (Err(e), false)
                    }
                };
                if tx.send(AppEvent::PlaygroundMore { result: statement, has_more }).await.is_err() {
                    return;
                }
            }
            PlaygroundControl::SwitchDb(new_dbname) => {
                paginated = None;
                let Some(base) = &conn_base else { continue };
                let new_dsn =
                    build_conninfo(&base.host, base.port, &base.user, &new_dbname, base.password.as_deref());
                match super::connect(&new_dsn, &tls).await {
                    Ok(new_client) => {
                        client = new_client;
                        if let Some(pid) = backend_pid(&client).await
                            && tx.send(AppEvent::PlaygroundPid(pid)).await.is_err()
                        {
                            return;
                        }
                    }
                    Err(e) => {
                        // Disclosed ceiling: if this reconnect fails, `client`
                        // stays pointed at the previous database until the
                        // next successful switch or app restart — surfaced
                        // once here, not silently swallowed.
                        let msg =
                            format!("playground connection failed to follow switch to '{new_dbname}': {e:#}");
                        if tx.send(AppEvent::PlaygroundResult { result: Err(msg), has_more: false }).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_collects_rows_between_columns_and_done() {
        let messages = vec![
            Msg::Columns(vec!["id".to_string()]),
            Msg::Row(vec![Some("1".to_string())]),
            Msg::Row(vec![Some("2".to_string())]),
            Msg::Done(2),
        ];
        assert_eq!(
            group(messages),
            vec![StatementResult::Rows {
                columns: vec!["id".to_string()],
                rows: vec![vec![Some("1".to_string())], vec![Some("2".to_string())]],
            }]
        );
    }

    #[test]
    fn group_command_only_statement_has_no_columns() {
        let messages = vec![Msg::Done(3)];
        assert_eq!(group(messages), vec![StatementResult::Command { rows_affected: 3 }]);
    }

    #[test]
    fn group_multi_statement_script_yields_one_result_per_statement() {
        let messages = vec![
            Msg::Columns(vec!["id".to_string()]),
            Msg::Row(vec![Some("1".to_string())]),
            Msg::Done(1),
            Msg::Done(2),
        ];
        assert_eq!(
            group(messages),
            vec![
                StatementResult::Rows { columns: vec!["id".to_string()], rows: vec![vec![Some("1".to_string())]] },
                StatementResult::Command { rows_affected: 2 },
            ]
        );
    }

    #[test]
    fn needs_confirmation_false_for_all_select_script() {
        assert!(!needs_confirmation("SELECT 1; SELECT 2;"));
    }

    #[test]
    fn needs_confirmation_true_when_one_statement_writes() {
        assert!(needs_confirmation("SELECT 1; DELETE FROM foo;"));
    }

    #[test]
    fn needs_confirmation_classification_is_case_and_whitespace_insensitive() {
        assert!(!needs_confirmation("  with cte as (select 1) select * from cte  "));
        assert!(needs_confirmation("  update foo set x = 1  "));
    }

    #[test]
    fn page_query_wraps_with_limit_offset() {
        assert_eq!(
            page_query("SELECT * FROM foo", 400),
            format!("SELECT * FROM (SELECT * FROM foo) AS pgpilot_page LIMIT {PLAYGROUND_PAGE_SIZE} OFFSET 400")
        );
    }

    #[test]
    fn is_paginatable_select_true_only_for_bare_select() {
        assert!(is_paginatable_select("SELECT 1"));
        assert!(is_paginatable_select("  select 1  "));
        assert!(!is_paginatable_select("WITH cte AS (SELECT 1) SELECT * FROM cte"));
        assert!(!is_paginatable_select("UPDATE foo SET x = 1"));
        assert!(!is_paginatable_select("(SELECT 1)"));
    }

    #[test]
    fn is_single_paginatable_select_none_for_multi_statement_script() {
        assert_eq!(is_single_paginatable_select("SELECT 1; SELECT 2;"), None);
    }

    #[test]
    fn is_single_paginatable_select_some_for_one_bare_select() {
        assert_eq!(is_single_paginatable_select(" select * from foo "), Some("select * from foo".to_string()));
    }
}
