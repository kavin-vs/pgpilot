use anyhow::Result;
use tokio_postgres::Client;

#[derive(Debug, Clone)]
pub struct ConnectionsData {
    pub max_connections: i32,
    pub active: i64,
    pub idle: i64,
    pub idle_in_txn: i64,
    pub used: i64,
}

const QUERY: &str = "
    SELECT
        (SELECT setting::int FROM pg_settings WHERE name = 'max_connections') AS max_connections,
        count(*) FILTER (WHERE state = 'active')                        AS active,
        count(*) FILTER (WHERE state = 'idle')                          AS idle,
        count(*) FILTER (WHERE state = 'idle in transaction'
                           OR state = 'idle in transaction (aborted)')  AS idle_in_txn,
        count(*)                                                        AS used
    FROM pg_stat_activity
";

pub async fn fetch(client: &Client) -> Result<ConnectionsData> {
    let row = client.query_one(QUERY, &[]).await?;
    Ok(ConnectionsData {
        max_connections: row.get("max_connections"),
        active: row.get("active"),
        idle: row.get("idle"),
        idle_in_txn: row.get("idle_in_txn"),
        used: row.get("used"),
    })
}
