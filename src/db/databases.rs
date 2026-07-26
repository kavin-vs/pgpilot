use anyhow::Result;
use tokio_postgres::Client;

/// Backs the database picker (`ui/picker.rs`), opened with `d`.
#[derive(Debug, Clone)]
pub struct DatabaseRow {
    pub name: String,
    pub owner: String,
    pub size_bytes: i64,
    pub sessions: i64,
    /// Cumulative counters — the picker computes a tps rate from the delta
    /// between consecutive polls, same treatment as the throughput chart.
    pub xact_commit: i64,
    pub xact_rollback: i64,
    pub cache_hit_pct: Option<f64>,
    pub is_template: bool,
}

const QUERY: &str = "
    SELECT
        d.datname AS name,
        pg_get_userbyid(d.datdba) AS owner,
        pg_database_size(d.datname) AS size_bytes,
        COALESCE(a.sessions, 0) AS sessions,
        COALESCE(sd.xact_commit, 0) AS xact_commit,
        COALESCE(sd.xact_rollback, 0) AS xact_rollback,
        CASE WHEN COALESCE(sd.blks_hit, 0) + COALESCE(sd.blks_read, 0) = 0 THEN NULL
             ELSE (COALESCE(sd.blks_hit, 0)::float8
                   / (COALESCE(sd.blks_hit, 0) + COALESCE(sd.blks_read, 0))::float8) * 100.0
        END AS cache_hit_pct,
        d.datistemplate AS is_template
    FROM pg_database d
    LEFT JOIN pg_stat_database sd ON sd.datname = d.datname
    LEFT JOIN (
        SELECT datname, count(*) AS sessions FROM pg_stat_activity GROUP BY datname
    ) a ON a.datname = d.datname
    WHERE d.datallowconn
    ORDER BY d.datname
";

impl DatabaseRow {
    /// Databases managed-Postgres providers reserve for internal use — not
    /// a template (`datistemplate`), but `pg_hba.conf` denies ordinary
    /// client connections regardless of role, so switching to one is a
    /// guaranteed FATAL (hit live against RDS's `rdsadmin`). Name-based
    /// heuristic against provider docs, not a catalog fact — a real user
    /// database that happens to share one of these names would be a false
    /// positive; known ceiling.
    const RESERVED_NAMES: [&'static str; 4] = ["rdsadmin", "azure_maintenance", "azuresu", "cloudsqladmin"];

    pub fn is_reserved(&self) -> bool {
        Self::RESERVED_NAMES.iter().any(|n| n.eq_ignore_ascii_case(&self.name))
    }

    /// Single source of truth for "can the picker switch to this row" —
    /// covers templates and reserved system databases alike, so a switch
    /// attempt never even tries a connection that's certain to fail.
    pub fn unswitchable_reason(&self) -> Option<&'static str> {
        if self.is_template {
            Some("template databases cannot be monitored")
        } else if self.is_reserved() {
            Some("reserved system databases cannot be monitored")
        } else {
            None
        }
    }
}

pub async fn fetch(client: &Client) -> Result<Vec<DatabaseRow>> {
    let rows = client.query(QUERY, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| DatabaseRow {
            name: row.get("name"),
            owner: row.get("owner"),
            size_bytes: row.get("size_bytes"),
            sessions: row.get("sessions"),
            xact_commit: row.get("xact_commit"),
            xact_rollback: row.get("xact_rollback"),
            cache_hit_pct: row.get("cache_hit_pct"),
            is_template: row.get("is_template"),
        })
        .collect())
}
