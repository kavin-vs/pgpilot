use anyhow::Result;
use tokio_postgres::Client;

/// Fetched once at connect/reconnect/db-switch, not on any polling tier —
/// none of this changes mid-session.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub version: String,
    /// `server_version_num` (e.g. `170010` for 17.10) — lets callers branch
    /// on catalog differences between major versions (see `db::cache_io`'s
    /// PG17+ `pg_stat_checkpointer` split) without a separate query.
    pub version_num: i32,
    pub uptime_secs: f64,
    pub current_db_owner: String,
}

const QUERY: &str = "
    SELECT
        current_setting('server_version') AS version,
        current_setting('server_version_num')::int AS version_num,
        EXTRACT(EPOCH FROM (now() - pg_postmaster_start_time()))::float8 AS uptime_secs,
        pg_get_userbyid(datdba) AS current_db_owner
    FROM pg_database
    WHERE datname = current_database()
";

pub async fn fetch(client: &Client) -> Result<ServerInfo> {
    let row = client.query_one(QUERY, &[]).await?;
    Ok(ServerInfo {
        version: row.get("version"),
        version_num: row.get("version_num"),
        uptime_secs: row.get("uptime_secs"),
        current_db_owner: row.get("current_db_owner"),
    })
}
