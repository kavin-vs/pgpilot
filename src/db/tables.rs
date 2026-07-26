use anyhow::Result;
use std::collections::BTreeMap;
use tokio_postgres::Client;

#[derive(Debug, Clone)]
pub struct TableRow {
    pub schema_name: String,
    pub table_name: String,
    pub total_bytes: i64,
    pub indexes_toast_bytes: i64,
    pub live_tuples: i64,
    pub dead_tuple_pct: Option<f64>,
    /// `age(relfrozenxid)` — how many transactions until wraparound vacuum
    /// is forced on this table.
    pub xid_age: i32,
    /// Cumulative counters — callers compute a per-hour rate from the delta
    /// between consecutive slow-tier polls.
    pub seq_scan: i64,
    pub idx_use_pct: Option<f64>,
    /// Seconds since the last autovacuum (or plain vacuum, if never
    /// autovacuumed); `None` if the table has never been vacuumed at all.
    pub last_vacuum_secs_ago: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct TablesData {
    pub tables: Vec<TableRow>,
    /// Total bytes per schema, summed client-side from `tables`.
    pub schema_totals: BTreeMap<String, i64>,
}

// Raw bytes are fetched (not pg_size_pretty) so the UI can sort by size;
// human-readable formatting happens client-side at display time. Same
// treatment for percentages (float8, not NUMERIC) and durations (EXTRACT
// EPOCH seconds, formatted client-side) as the rest of the db layer.
const QUERY: &str = "
    SELECT
        n.nspname AS schema_name,
        c.relname AS table_name,
        pg_total_relation_size(c.oid) AS total_bytes,
        pg_total_relation_size(c.oid) - pg_relation_size(c.oid) AS indexes_toast_bytes,
        COALESCE(s.n_live_tup, 0) AS live_tuples,
        CASE WHEN COALESCE(s.n_live_tup, 0) + COALESCE(s.n_dead_tup, 0) = 0 THEN NULL
             ELSE (COALESCE(s.n_dead_tup, 0)::float8
                   / (COALESCE(s.n_live_tup, 0) + COALESCE(s.n_dead_tup, 0))::float8) * 100.0
        END AS dead_tuple_pct,
        age(c.relfrozenxid) AS xid_age,
        COALESCE(s.seq_scan, 0) AS seq_scan,
        CASE WHEN COALESCE(s.seq_scan, 0) + COALESCE(s.idx_scan, 0) = 0 THEN NULL
             ELSE (COALESCE(s.idx_scan, 0)::float8
                   / (COALESCE(s.seq_scan, 0) + COALESCE(s.idx_scan, 0))::float8) * 100.0
        END AS idx_use_pct,
        EXTRACT(EPOCH FROM (now() - COALESCE(s.last_autovacuum, s.last_vacuum)))::float8 AS last_vacuum_secs_ago
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_stat_user_tables s ON s.relid = c.oid
    WHERE c.relkind IN ('r', 'p')
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
    ORDER BY total_bytes DESC
";

pub async fn fetch(client: &Client) -> Result<TablesData> {
    let rows = client.query(QUERY, &[]).await?;
    let tables: Vec<TableRow> = rows
        .iter()
        .map(|row| TableRow {
            schema_name: row.get("schema_name"),
            table_name: row.get("table_name"),
            total_bytes: row.get("total_bytes"),
            indexes_toast_bytes: row.get("indexes_toast_bytes"),
            live_tuples: row.get("live_tuples"),
            dead_tuple_pct: row.get("dead_tuple_pct"),
            xid_age: row.get("xid_age"),
            seq_scan: row.get("seq_scan"),
            idx_use_pct: row.get("idx_use_pct"),
            last_vacuum_secs_ago: row.get("last_vacuum_secs_ago"),
        })
        .collect();

    let mut schema_totals: BTreeMap<String, i64> = BTreeMap::new();
    for t in &tables {
        *schema_totals.entry(t.schema_name.clone()).or_insert(0) += t.total_bytes;
    }

    Ok(TablesData {
        tables,
        schema_totals,
    })
}
