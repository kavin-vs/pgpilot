use anyhow::Result;
use tokio_postgres::Client;

#[derive(Debug, Clone)]
pub struct IndexRow {
    pub schema_name: String,
    pub table_name: String,
    pub index_name: String,
    pub is_valid: bool,
    pub is_ready: bool,
    pub idx_scan: i64,
    pub index_size_bytes: i64,
}

// LEFT JOIN on pg_stat_user_indexes matters: invalid/not-ready indexes (e.g.
// from a failed CREATE INDEX CONCURRENTLY) may not have stats rows yet.
const QUERY: &str = "
    SELECT
        n.nspname AS schema_name,
        t.relname AS table_name,
        ic.relname AS index_name,
        i.indisvalid AS is_valid,
        i.indisready AS is_ready,
        COALESCE(s.idx_scan, 0) AS idx_scan,
        pg_relation_size(ic.oid) AS index_size_bytes
    FROM pg_index i
    JOIN pg_class ic ON ic.oid = i.indexrelid
    JOIN pg_class t  ON t.oid  = i.indrelid
    JOIN pg_namespace n ON n.oid = t.relnamespace
    LEFT JOIN pg_stat_user_indexes s ON s.indexrelid = i.indexrelid
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
    ORDER BY (NOT i.indisvalid) DESC, (NOT i.indisready) DESC, idx_scan ASC
";

pub async fn fetch(client: &Client) -> Result<Vec<IndexRow>> {
    let rows = client.query(QUERY, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| IndexRow {
            schema_name: row.get("schema_name"),
            table_name: row.get("table_name"),
            index_name: row.get("index_name"),
            is_valid: row.get("is_valid"),
            is_ready: row.get("is_ready"),
            idx_scan: row.get("idx_scan"),
            index_size_bytes: row.get("index_size_bytes"),
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct UnindexedForeignKey {
    pub schema_name: String,
    pub table_name: String,
    /// Comma-joined column names, in constraint order.
    pub columns: String,
}

// Heuristic, not a guarantee: flags a foreign key as unindexed unless some
// index leads with its first column and covers all its columns. Multi-column
// FKs backed by a differently-ordered index can still false-positive here —
// a known ceiling, same tier of accuracy as the other catalog-derived
// heuristics in this app.
const UNINDEXED_FK_QUERY: &str = "
    SELECT
        n.nspname AS schema_name,
        t.relname AS table_name,
        (
            SELECT string_agg(a.attname, ', ' ORDER BY x.ord)
            FROM unnest(c.conkey) WITH ORDINALITY AS x(attnum, ord)
            JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = x.attnum
        ) AS columns
    FROM pg_constraint c
    JOIN pg_class t ON t.oid = c.conrelid
    JOIN pg_namespace n ON n.oid = t.relnamespace
    WHERE c.contype = 'f'
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND NOT EXISTS (
          SELECT 1
          FROM pg_index i
          WHERE i.indrelid = c.conrelid
            AND i.indkey[0] = c.conkey[1]
            AND c.conkey <@ (i.indkey::int2[])::int2[]
      )
    ORDER BY schema_name, table_name
";

pub async fn fetch_unindexed_foreign_keys(client: &Client) -> Result<Vec<UnindexedForeignKey>> {
    let rows = client.query(UNINDEXED_FK_QUERY, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| UnindexedForeignKey {
            schema_name: row.get("schema_name"),
            table_name: row.get("table_name"),
            columns: row.get("columns"),
        })
        .collect())
}
