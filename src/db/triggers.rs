use anyhow::Result;
use tokio_postgres::Client;

#[derive(Debug, Clone)]
pub struct TriggerRow {
    pub schema_name: String,
    pub table_name: String,
    pub trigger_name: String,
    pub function_name: String,
    pub enabled: bool,
    /// Full `CREATE TRIGGER ...` DDL (timing/events already spelled out by
    /// Postgres) — simpler and more correct than hand-decoding `tgtype`.
    pub trigger_def: String,
    // ponytail: refetches every function body every 5 min even though only
    // one is ever shown at a time in the popup — fine while trigger counts/
    // function bodies stay small. If that stops being true, move this to an
    // on-demand fetch fired only when the popup opens (mirrors Cancel/
    // Terminate's single-shot PollControl shape in db/mod.rs).
    /// The function's full `CREATE OR REPLACE FUNCTION ...` body — what
    /// actually runs when the trigger fires; shown in the detail popup.
    pub function_def: String,
}

// tgisinternal excludes triggers Postgres auto-generates to enforce FOREIGN
// KEY constraints (RI_FKey_* functions) — not user-facing, would otherwise
// add one/two rows per FK.
const QUERY: &str = "
    SELECT
        n.nspname AS schema_name,
        c.relname AS table_name,
        t.tgname AS trigger_name,
        p.proname AS function_name,
        t.tgenabled != 'D' AS enabled,
        pg_get_triggerdef(t.oid) AS trigger_def,
        pg_get_functiondef(p.oid) AS function_def
    FROM pg_trigger t
    JOIN pg_class c ON c.oid = t.tgrelid
    JOIN pg_namespace n ON n.oid = c.relnamespace
    JOIN pg_proc p ON p.oid = t.tgfoid
    WHERE NOT t.tgisinternal
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
    ORDER BY schema_name, table_name, trigger_name
";

pub async fn fetch(client: &Client) -> Result<Vec<TriggerRow>> {
    let rows = client.query(QUERY, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| TriggerRow {
            schema_name: row.get("schema_name"),
            table_name: row.get("table_name"),
            trigger_name: row.get("trigger_name"),
            function_name: row.get("function_name"),
            enabled: row.get("enabled"),
            trigger_def: row.get("trigger_def"),
            function_def: row.get("function_def"),
        })
        .collect())
}
