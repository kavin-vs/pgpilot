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
    pub shared_blks_written: i64,
    pub temp_blks_read: i64,
    pub temp_blks_written: i64,
    /// `blk_read_time + blk_write_time` — 0.0 unless `track_io_timing = on`
    /// (off by default), which the UI detects by an all-rows-zero check rather
    /// than a second `SHOW` query.
    pub io_time_ms: f64,
    pub cache_hit_pct: Option<f64>,
}

impl StatementRow {
    /// Blocks that actually moved to/from disk — `shared_blks_hit` is
    /// deliberately excluded (a hit never touched a device). x8KB = bytes.
    pub fn io_blocks(&self) -> i64 {
        self.shared_blks_read + self.shared_blks_written + self.temp_blks_read + self.temp_blks_written
    }

    /// ponytail: assumes the 8KB default `block_size`; read `current_setting('block_size')`
    /// at connect if a non-default build ever needs exact bytes.
    pub fn io_bytes(&self) -> i64 {
        self.io_blocks() * 8192
    }
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
/// PG17 renamed the I/O timing columns (`blk_read_time` -> `shared_blk_read_time`),
/// same version skew `cache_io::fetch_bgwriter` already handles — built with
/// `format!` rather than two near-identical consts so the 20 lines they'd share
/// can't drift apart.
///
/// The row cut is a UNION of top-by-time and top-by-I/O: with a single
/// `ORDER BY total_exec_time DESC LIMIT 100`, a query that reads a lot of disk
/// but is cheap on wall time never made it into the result at all.
fn query(pg17_plus: bool) -> String {
    let io_time = if pg17_plus {
        "shared_blk_read_time + shared_blk_write_time"
    } else {
        "blk_read_time + blk_write_time"
    };
    format!(
        "
    WITH s AS (
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
            shared_blks_written,
            temp_blks_read,
            temp_blks_written,
            {io_time} AS io_time_ms,
            shared_blks_read + shared_blks_written + temp_blks_read + temp_blks_written AS io_blocks,
            CASE WHEN shared_blks_hit + shared_blks_read = 0 THEN NULL
                 ELSE (shared_blks_hit::float8 / (shared_blks_hit + shared_blks_read)::float8) * 100.0
            END AS cache_hit_pct
        FROM pg_stat_statements
        WHERE queryid IS NOT NULL
    )
    (SELECT * FROM s ORDER BY total_exec_time_ms DESC LIMIT 100)
    UNION
    (SELECT * FROM s ORDER BY io_blocks DESC LIMIT 50)
"
    )
}

/// Checked once at connect/reconnect/db-switch time and cached by the
/// caller, so the medium poll tier can skip querying entirely (not just
/// skip acting on the result) once it's known the extension isn't loaded.
pub async fn extension_loaded(client: &Client) -> Result<bool> {
    Ok(client.query_opt(EXTENSION_CHECK, &[]).await?.is_some())
}

pub async fn fetch(client: &Client, pg17_plus: bool) -> Result<Vec<StatementRow>> {
    let rows = client.query(&query(pg17_plus), &[]).await?;
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
            shared_blks_written: row.get("shared_blks_written"),
            temp_blks_read: row.get("temp_blks_read"),
            temp_blks_written: row.get("temp_blks_written"),
            io_time_ms: row.get("io_time_ms"),
            cache_hit_pct: row.get("cache_hit_pct"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(hit: i64, read: i64, written: i64, temp_read: i64, temp_written: i64) -> StatementRow {
        StatementRow {
            query_id: 1,
            query: String::new(),
            calls: 1,
            total_exec_time_ms: 0.0,
            mean_exec_time_ms: 0.0,
            stddev_exec_time_ms: 0.0,
            rows: 0,
            shared_blks_hit: hit,
            shared_blks_read: read,
            shared_blks_written: written,
            temp_blks_read: temp_read,
            temp_blks_written: temp_written,
            io_time_ms: 0.0,
            cache_hit_pct: None,
        }
    }

    #[test]
    fn io_blocks_sums_disk_traffic_and_ignores_cache_hits() {
        assert_eq!(row(9_999, 3, 4, 5, 6).io_blocks(), 18);
        assert_eq!(row(9_999, 0, 0, 0, 0).io_blocks(), 0);
        assert_eq!(row(0, 1, 0, 0, 0).io_bytes(), 8192);
    }
}
