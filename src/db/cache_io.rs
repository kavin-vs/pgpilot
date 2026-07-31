use anyhow::Result;
use tokio_postgres::Client;

#[derive(Debug, Clone)]
pub struct CacheOverall {
    pub blks_hit: i64,
    pub blks_read: i64,
    /// None when the cluster has no reads yet (e.g. freshly restarted).
    pub hit_ratio_pct: Option<f64>,
    /// Cumulative counters (not rates) — callers compute commits/s and
    /// rollback% from the delta between consecutive polls.
    pub xact_commit: i64,
    pub xact_rollback: i64,
    pub temp_files: i64,
    pub temp_bytes: i64,
}

#[derive(Debug, Clone)]
pub struct CacheDbRow {
    pub datname: String,
    pub hit_ratio_pct: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ColdRelation {
    pub schema_name: String,
    pub table_name: String,
    pub heap_blks_hit: i64,
    pub heap_blks_read: i64,
    pub hit_ratio_pct: Option<f64>,
    pub size_bytes: i64,
}

/// Checkpointer/background-writer stats. PG17 split the checkpointer
/// columns out of `pg_stat_bgwriter` into a new `pg_stat_checkpointer` view
/// (`checkpoints_timed`/`checkpoints_req`/`buffers_checkpoint` became
/// `num_timed`/`num_requested`/`buffers_written` there) — `fetch_bgwriter()`
/// below queries the right view for the connected server and maps both
/// shapes into this same struct. `buffers_backend` moved to `pg_stat_io` on
/// PG17+, which needs a heavier per-backend-type aggregation to reconstruct;
/// not done here, so it's `None` on PG17+ rather than a fabricated `0`.
#[derive(Debug, Clone, Default)]
pub struct BgWriterStats {
    pub checkpoints_timed: i64,
    pub checkpoints_req: i64,
    pub buffers_checkpoint: i64,
    pub buffers_clean: i64,
    pub buffers_backend: Option<i64>,
    pub buffers_alloc: i64,
    pub maxwritten_clean: i64,
}

#[derive(Debug, Clone)]
pub struct ReplicationRow {
    pub application_name: String,
    /// Bytes the named replica is behind. Only meaningful when connected to
    /// a primary — `pg_current_wal_lsn()` errors on a standby, but this
    /// query only evaluates it per matched row, so a standby with no
    /// downstream replicas of its own (the common case) returns 0 rows and
    /// never hits that error.
    pub lag_bytes: Option<i64>,
}

// Percentages computed as float8 (not NUMERIC) so they map directly to f64
// without a decimal crate; display-time rounding happens in the UI.
const OVERALL_QUERY: &str = "
    SELECT
        sum(blks_hit)::bigint AS blks_hit,
        sum(blks_read)::bigint AS blks_read,
        CASE WHEN sum(blks_hit) + sum(blks_read) = 0 THEN NULL
             ELSE (sum(blks_hit)::float8 / (sum(blks_hit) + sum(blks_read))::float8) * 100.0
        END AS hit_ratio_pct,
        sum(xact_commit)::bigint AS xact_commit,
        sum(xact_rollback)::bigint AS xact_rollback,
        sum(temp_files)::bigint AS temp_files,
        sum(temp_bytes)::bigint AS temp_bytes
    FROM pg_stat_database
";

const PER_DATABASE_QUERY: &str = "
    SELECT
        datname,
        CASE WHEN blks_hit + blks_read = 0 THEN NULL
             ELSE (blks_hit::float8 / (blks_hit + blks_read)::float8) * 100.0
        END AS hit_ratio_pct
    FROM pg_stat_database
    WHERE datname IS NOT NULL
    ORDER BY hit_ratio_pct NULLS LAST
";

// Capped at 15 and restricted to relations with actual I/O activity — this
// list isn't scrollable, so it stays short by construction.
const COLDEST_QUERY: &str = "
    SELECT
        n.nspname AS schema_name,
        c.relname AS table_name,
        COALESCE(s.heap_blks_hit, 0) AS heap_blks_hit,
        COALESCE(s.heap_blks_read, 0) AS heap_blks_read,
        CASE WHEN COALESCE(s.heap_blks_hit, 0) + COALESCE(s.heap_blks_read, 0) = 0 THEN NULL
             ELSE (COALESCE(s.heap_blks_hit, 0)::float8
                   / (COALESCE(s.heap_blks_hit, 0) + COALESCE(s.heap_blks_read, 0))::float8) * 100.0
        END AS hit_ratio_pct,
        pg_total_relation_size(c.oid) AS size_bytes
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_statio_user_tables s ON s.relid = c.oid
    WHERE c.relkind IN ('r', 'p')
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND COALESCE(s.heap_blks_hit, 0) + COALESCE(s.heap_blks_read, 0) > 0
    ORDER BY hit_ratio_pct ASC NULLS LAST
    LIMIT 15
";

const BGWRITER_QUERY_PRE17: &str = "
    SELECT checkpoints_timed, checkpoints_req, buffers_checkpoint,
           buffers_clean, buffers_backend, buffers_alloc, maxwritten_clean
    FROM pg_stat_bgwriter
";

// Both are single-row views (no join key needed) — a comma-join yields
// exactly one combined row.
const BGWRITER_QUERY_PG17_PLUS: &str = "
    SELECT
        cp.num_timed AS checkpoints_timed,
        cp.num_requested AS checkpoints_req,
        cp.buffers_written AS buffers_checkpoint,
        bg.buffers_clean,
        bg.maxwritten_clean,
        bg.buffers_alloc
    FROM pg_stat_checkpointer cp, pg_stat_bgwriter bg
";

const REPLICATION_QUERY: &str = "
    SELECT application_name, pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn)::bigint AS lag_bytes
    FROM pg_stat_replication
";

/// Each of these blocks (rendered on Overview) is fetched (and can fail)
/// independently — one broken query shouldn't blank out the other three,
/// and each block shows its own error state rather than the whole tab going dark.
pub async fn fetch_overall(client: &Client) -> Result<CacheOverall> {
    let row = client.query_one(OVERALL_QUERY, &[]).await?;
    Ok(CacheOverall {
        blks_hit: row.get("blks_hit"),
        blks_read: row.get("blks_read"),
        hit_ratio_pct: row.get("hit_ratio_pct"),
        xact_commit: row.get("xact_commit"),
        xact_rollback: row.get("xact_rollback"),
        temp_files: row.get("temp_files"),
        temp_bytes: row.get("temp_bytes"),
    })
}

pub async fn fetch_per_database(client: &Client) -> Result<Vec<CacheDbRow>> {
    let rows = client.query(PER_DATABASE_QUERY, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| CacheDbRow {
            datname: row.get("datname"),
            hit_ratio_pct: row.get("hit_ratio_pct"),
        })
        .collect())
}

pub async fn fetch_coldest(client: &Client) -> Result<Vec<ColdRelation>> {
    let rows = client.query(COLDEST_QUERY, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| ColdRelation {
            schema_name: row.get("schema_name"),
            table_name: row.get("table_name"),
            heap_blks_hit: row.get("heap_blks_hit"),
            heap_blks_read: row.get("heap_blks_read"),
            hit_ratio_pct: row.get("hit_ratio_pct"),
            size_bytes: row.get("size_bytes"),
        })
        .collect())
}

pub async fn fetch_bgwriter(client: &Client, pg17_plus: bool) -> Result<BgWriterStats> {
    if pg17_plus {
        let row = client.query_one(BGWRITER_QUERY_PG17_PLUS, &[]).await?;
        Ok(BgWriterStats {
            checkpoints_timed: row.get("checkpoints_timed"),
            checkpoints_req: row.get("checkpoints_req"),
            buffers_checkpoint: row.get("buffers_checkpoint"),
            buffers_clean: row.get("buffers_clean"),
            buffers_backend: None,
            buffers_alloc: row.get("buffers_alloc"),
            maxwritten_clean: row.get("maxwritten_clean"),
        })
    } else {
        let row = client.query_one(BGWRITER_QUERY_PRE17, &[]).await?;
        Ok(BgWriterStats {
            checkpoints_timed: row.get("checkpoints_timed"),
            checkpoints_req: row.get("checkpoints_req"),
            buffers_checkpoint: row.get("buffers_checkpoint"),
            buffers_clean: row.get("buffers_clean"),
            buffers_backend: Some(row.get("buffers_backend")),
            buffers_alloc: row.get("buffers_alloc"),
            maxwritten_clean: row.get("maxwritten_clean"),
        })
    }
}

pub async fn fetch_replication(client: &Client) -> Result<Vec<ReplicationRow>> {
    let rows = client.query(REPLICATION_QUERY, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| ReplicationRow {
            application_name: row.get("application_name"),
            lag_bytes: row.get("lag_bytes"),
        })
        .collect())
}
