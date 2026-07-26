use anyhow::Result;
use tokio_postgres::Client;

#[derive(Debug, Clone)]
pub struct ActivityRow {
    pub pid: i32,
    pub username: Option<String>,
    pub state: Option<String>,
    pub duration_secs: Option<f64>,
    pub wait_event_type: Option<String>,
    pub wait_event: Option<String>,
    pub query: Option<String>,
    /// From `pg_blocking_pids(pid)` (built into Postgres since 9.6) — no
    /// hand-rolled `pg_locks` self-join needed to build the blocking tree.
    pub blocked_by: Vec<i32>,
}

#[derive(Debug, Clone)]
pub struct ActivityData {
    pub rows: Vec<ActivityRow>,
}

// Query text truncated server-side (left(query, 220)) so a busy activity
// table can't balloon the result set on wide/bulk statements. Restricted to
// `state IS NOT NULL`: Postgres sets `state` only for backends with a real
// query/transaction lifecycle (client backends, an autovacuum worker
// actively running) — background maintenance processes (checkpointer,
// bgwriter, walwriter, autovacuum launcher) have `state = NULL` and no
// query/state_change timestamp, so `duration_secs` falls back to their
// entire process uptime and would otherwise dominate the DESC sort ahead of
// genuinely long-running queries. Nothing user-actionable lives there
// anyway (canceling the checkpointer is a no-op Postgres just restarts).
const QUERY: &str = "
    SELECT
        pid,
        usename AS username,
        state,
        EXTRACT(EPOCH FROM (now() - COALESCE(state_change, query_start, xact_start, backend_start)))::float8 AS duration_secs,
        wait_event_type,
        wait_event,
        left(query, 220) AS query,
        pg_blocking_pids(pid) AS blocked_by
    FROM pg_stat_activity
    WHERE pid <> pg_backend_pid() AND state IS NOT NULL
    ORDER BY duration_secs DESC NULLS LAST
";

pub async fn fetch(client: &Client) -> Result<ActivityData> {
    let rows = client
        .query(QUERY, &[])
        .await?
        .iter()
        .map(|row| ActivityRow {
            pid: row.get("pid"),
            username: row.get("username"),
            state: row.get("state"),
            duration_secs: row.get("duration_secs"),
            wait_event_type: row.get("wait_event_type"),
            wait_event: row.get("wait_event"),
            query: row.get("query"),
            blocked_by: row.get("blocked_by"),
        })
        .collect();

    Ok(ActivityData { rows })
}
