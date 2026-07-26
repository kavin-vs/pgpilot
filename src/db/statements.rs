use anyhow::Result;
use tokio_postgres::Client;

#[derive(Debug, Clone)]
pub struct StatementRow {
    pub query_id: i64,
    pub query: String,
    pub calls: i64,
    pub total_exec_time_ms: f64,
    pub mean_exec_time_ms: f64,
    pub stddev_exec_time_ms: f64,
    pub rows: i64,
    pub shared_blks_hit: i64,
    pub shared_blks_read: i64,
    pub cache_hit_pct: Option<f64>,
}

/// Enum, not `Option<Option<T>>` or a bool+Option pair — makes the "extension
/// not loaded" degrade path unambiguous at every call site.
#[derive(Debug, Clone)]
pub enum StatementsData {
    NotAvailable,
    Available(Vec<StatementRow>),
}

const EXTENSION_CHECK: &str = "SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements'";

// Column names (total_exec_time/mean_exec_time/stddev_exec_time) match
// PG13+; earlier versions used total_time/mean_time/stddev_time — a version
// floor, not handled here (no minimum PG version is otherwise documented for
// this app).
const QUERY: &str = "
    SELECT
        queryid AS query_id,
        query,
        calls,
        total_exec_time AS total_exec_time_ms,
        mean_exec_time AS mean_exec_time_ms,
        stddev_exec_time AS stddev_exec_time_ms,
        rows,
        shared_blks_hit,
        shared_blks_read,
        CASE WHEN shared_blks_hit + shared_blks_read = 0 THEN NULL
             ELSE (shared_blks_hit::float8 / (shared_blks_hit + shared_blks_read)::float8) * 100.0
        END AS cache_hit_pct
    FROM pg_stat_statements
    WHERE queryid IS NOT NULL
    ORDER BY total_exec_time DESC
    LIMIT 100
";

/// Checked once at connect/reconnect/db-switch time and cached by the
/// caller, so the medium poll tier can skip querying entirely (not just
/// skip acting on the result) once it's known the extension isn't loaded.
pub async fn extension_loaded(client: &Client) -> Result<bool> {
    Ok(client.query_opt(EXTENSION_CHECK, &[]).await?.is_some())
}

pub async fn fetch(client: &Client) -> Result<Vec<StatementRow>> {
    let rows = client.query(QUERY, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| StatementRow {
            query_id: row.get("query_id"),
            query: row.get("query"),
            calls: row.get("calls"),
            total_exec_time_ms: row.get("total_exec_time_ms"),
            mean_exec_time_ms: row.get("mean_exec_time_ms"),
            stddev_exec_time_ms: row.get("stddev_exec_time_ms"),
            rows: row.get("rows"),
            shared_blks_hit: row.get("shared_blks_hit"),
            shared_blks_read: row.get("shared_blks_read"),
            cache_hit_pct: row.get("cache_hit_pct"),
        })
        .collect())
}
