use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, Notify};
use tokio::time::MissedTickBehavior;
use tokio_postgres::Client;

use crate::cli::{build_conninfo, ConnParts};
use crate::event::{AppEvent, PanelSnapshot, StatusLevel};

pub mod activity;
pub mod cache_io;
pub mod connections;
pub mod databases;
pub mod indexes;
pub mod serverinfo;
pub mod statements;
pub mod tables;
pub mod tls;

use statements::StatementsData;
use tls::TlsMode;

/// Commands the render loop sends to `poll_task` over the control channel —
/// switching database, adjusting the fast-tier poll rate, pausing, and
/// cancel/terminate (which must run on the task's own `Client`; the render
/// loop can't reach it directly).
pub enum PollControl {
    SwitchDb(String),
    SetInterval(Duration),
    TogglePause,
    Cancel(i32),
    Terminate(i32),
}

const MEDIUM_INTERVAL: Duration = Duration::from_secs(15);
const SLOW_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Connects to Postgres and spawns the connection driver task in the
/// background. Without spawning this task, `Client` silently stops working
/// as soon as the returned `Connection` future is dropped.
///
/// The driver task's errors are intentionally not logged here: while the TUI
/// is running it owns the alternate screen, and writing to stderr directly
/// would corrupt the display. A dead connection surfaces naturally the next
/// time `poll_task` tries to use the client and reports it via `AppEvent::Error`.
pub async fn connect(conninfo: &str, tls: &TlsMode) -> Result<Client> {
    match tls {
        TlsMode::Disabled => {
            let (client, connection) =
                tokio_postgres::connect(conninfo, tokio_postgres::NoTls).await?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            Ok(client)
        }
        TlsMode::Enabled {
            root_cert,
            client_cert,
            client_key,
        } => {
            let connector = tls::make_connector(
                root_cert.as_deref(),
                client_cert.as_deref(),
                client_key.as_deref(),
            )?;
            let (client, connection) = tokio_postgres::connect(conninfo, connector).await?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            Ok(client)
        }
    }
}

/// Sends one panel's fetch result, labeling any error with which query it
/// came from (surfaced in the error banner/detail view — otherwise a bare
/// Postgres error gives no clue which of the tier's several queries broke).
/// Returns `(channel_still_open, connection_looks_dead)` — the latter is
/// `true` only when the underlying `tokio_postgres::Error` reports the
/// connection itself closed (`Error::is_closed()`), not for an ordinary
/// query-level failure (wrong column, permission denied, etc.), so a single
/// broken query can't trigger a pointless reconnect loop that just fails the
/// same query again.
async fn send_labeled(tx: &mpsc::Sender<AppEvent>, label: &str, result: Result<PanelSnapshot>) -> (bool, bool) {
    match result {
        Ok(snapshot) => (tx.send(AppEvent::Snapshot(snapshot)).await.is_ok(), false),
        Err(e) => {
            let dead = e
                .downcast_ref::<tokio_postgres::Error>()
                .is_some_and(tokio_postgres::Error::is_closed);
            let open = tx
                .send(AppEvent::Error {
                    source: label.to_string(),
                    // `{e}`/`e.to_string()` only shows anyhow's outer wrapper
                    // (for a Postgres error, tokio-postgres's generic "db
                    // error"/"error communicating with database" kind label)
                    // — the actual useful text (the server's ERROR/DETAIL/
                    // HINT) is one level down in the cause chain, which only
                    // the alternate `{:#}` format includes.
                    message: format!("{e:#}"),
                })
                .await
                .is_ok();
            (open, dead)
        }
    }
}

/// Counters/activity — the only tier the `-`/`+` rate keys touch. Each of
/// the 9 queries is sent independently: one failing (e.g. a version-specific
/// column missing) no longer blocks the others from updating, which used to
/// leave the whole dashboard (or, for the 4 Cache & I/O queries, the whole
/// tab) looking dead over one broken query. `pg17_plus` picks the right
/// checkpointer query (see `cache_io::fetch_bgwriter`).
async fn send_fast(client: &Client, tx: &mpsc::Sender<AppEvent>, pg17_plus: bool) -> (bool, bool) {
    let results: [(&str, Result<PanelSnapshot>); 9] = [
        ("connections", connections::fetch(client).await.map(PanelSnapshot::Connections)),
        ("cache overview", cache_io::fetch_overall(client).await.map(PanelSnapshot::CacheOverall)),
        (
            "per-database cache",
            cache_io::fetch_per_database(client).await.map(PanelSnapshot::CachePerDatabase),
        ),
        ("coldest relations", cache_io::fetch_coldest(client).await.map(PanelSnapshot::CacheColdest)),
        (
            "checkpoints & wal",
            cache_io::fetch_bgwriter(client, pg17_plus).await.map(PanelSnapshot::CacheCheckpoints),
        ),
        ("replication", cache_io::fetch_replication(client).await.map(PanelSnapshot::CacheReplication)),
        ("activity", activity::fetch(client).await.map(PanelSnapshot::Activity)),
        ("indexes", indexes::fetch(client).await.map(PanelSnapshot::Indexes)),
        ("databases", databases::fetch(client).await.map(PanelSnapshot::Databases)),
    ];

    let mut channel_open = true;
    let mut connection_dead = false;
    for (label, result) in results {
        let (open, dead) = send_labeled(tx, label, result).await;
        channel_open &= open;
        connection_dead |= dead;
    }
    (channel_open, connection_dead)
}

/// `pg_stat_statements`, fixed 15s — skipped (cheaply, no query at all) when
/// the extension isn't loaded.
async fn send_medium(client: &Client, tx: &mpsc::Sender<AppEvent>, has_pg_stat_statements: bool) -> bool {
    let data = if has_pg_stat_statements {
        statements::fetch(client).await.map(StatementsData::Available)
    } else {
        Ok(StatementsData::NotAvailable)
    };
    send_labeled(tx, "pg_stat_statements", data.map(PanelSnapshot::Statements)).await.0
}

/// Catalog-heavy: dead-tuple %, xid age, unindexed FKs — fixed 5 minutes.
/// Same per-query isolation as `send_fast`.
async fn send_slow(client: &Client, tx: &mpsc::Sender<AppEvent>) -> bool {
    let results: [(&str, Result<PanelSnapshot>); 2] = [
        ("tables", tables::fetch(client).await.map(PanelSnapshot::Tables)),
        (
            "unindexed foreign keys",
            indexes::fetch_unindexed_foreign_keys(client).await.map(PanelSnapshot::UnindexedForeignKeys),
        ),
    ];

    let mut channel_open = true;
    for (label, result) in results {
        channel_open &= send_labeled(tx, label, result).await.0;
    }
    channel_open
}

/// Fast-tier fetch with the original single-tier `poll_task`'s error
/// handling: report each failing query, then attempt one reconnect if any of
/// them indicates the connection itself is dead (see `send_labeled`),
/// retried again on the next tick if that also fails. Shared by both the
/// fast ticker and the `r`-key manual refresh (which bypasses pause but not
/// this error path). Returns `false` if the event channel is closed,
/// meaning the caller should stop the task.
async fn poll_fast_with_reconnect(
    client: &mut Client,
    dsn: &str,
    tls: &TlsMode,
    tx: &mpsc::Sender<AppEvent>,
    has_pg_stat_statements: &mut bool,
    pg17_plus: bool,
) -> bool {
    let (channel_open, connection_dead) = send_fast(client, tx, pg17_plus).await;
    if connection_dead
        && let Ok(new_client) = connect(dsn, tls).await
    {
        *has_pg_stat_statements = statements::extension_loaded(&new_client).await.unwrap_or(false);
        *client = new_client;
    }
    channel_open
}

/// `tokio_postgres::Error`'s bare `Display` shows only its generic top-level
/// label ("db error") — the actual server ERROR/DETAIL/HINT text lives in
/// its `source()` chain. Unlike `anyhow::Error` (whose alternate `{:#}`
/// format walks the chain for us), a raw `tokio_postgres::Error` needs this
/// walked by hand.
fn chained_message(e: &tokio_postgres::Error) -> String {
    let mut parts = vec![e.to_string()];
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        parts.push(s.to_string());
        source = s.source();
    }
    parts.join(": ")
}

async fn run_signal(client: &Client, tx: &mpsc::Sender<AppEvent>, func: &str, pid: i32) -> bool {
    let sql = format!("SELECT {func}($1)");
    match client.execute(&sql, &[&pid]).await {
        Ok(_) => tx
            .send(AppEvent::Status(format!("{func}({pid}) -> ok"), StatusLevel::Info))
            .await
            .is_ok(),
        Err(e) => tx
            .send(AppEvent::Error {
                source: func.to_string(),
                message: format!("pid {pid}: {}", chained_message(&e)),
            })
            .await
            .is_ok(),
    }
}

/// Background polling loop: owns the DB client, refetches panels across
/// three tiers (fast/medium/slow — see `fetch_fast`/`fetch_medium`/
/// `fetch_slow`), or immediately when `refresh_now` is notified (e.g. by the
/// 'r' key, which bypasses `paused`). Never panics or exits the process on a
/// query/connection error — it reports via `AppEvent::Error` and retries.
///
/// `conn_base` (host/port/user/password, `dbname` excluded) enables the 'd'
/// popup's database-switching: when `control_rx` receives `SwitchDb`, `dsn`
/// is rebuilt against that database and reconnected. `None` (raw `--dsn`
/// connections, an opaque string we can't safely take `dbname` out of) means
/// the switch request is silently ignored — the UI never offers the popup in
/// that case (`App::can_switch_db`), so nothing should arrive.
#[allow(clippy::too_many_arguments)]
pub async fn poll_task(
    mut client: Client,
    mut dsn: String,
    tls: TlsMode,
    conn_base: Option<ConnParts>,
    fast_interval: Duration,
    tx: mpsc::Sender<AppEvent>,
    refresh_now: Arc<Notify>,
    mut control_rx: mpsc::Receiver<PollControl>,
) {
    let mut fast_ticker = tokio::time::interval(fast_interval);
    fast_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut medium_ticker = tokio::time::interval(MEDIUM_INTERVAL);
    medium_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut slow_ticker = tokio::time::interval(SLOW_INTERVAL);
    slow_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut paused = false;
    let mut has_pg_stat_statements = statements::extension_loaded(&client).await.unwrap_or(false);
    // Defaults to pre-17 behavior if detection fails — matches the query
    // shape this app used before PG17 support existed.
    let mut pg17_plus = false;

    if let Ok(info) = serverinfo::fetch(&client).await {
        pg17_plus = info.version_num >= 170_000;
        if tx.send(AppEvent::ServerInfo(info)).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            _ = fast_ticker.tick() => {
                if paused { continue; }
                if !poll_fast_with_reconnect(&mut client, &dsn, &tls, &tx, &mut has_pg_stat_statements, pg17_plus).await {
                    return;
                }
            }
            _ = medium_ticker.tick() => {
                if paused { continue; }
                if !send_medium(&client, &tx, has_pg_stat_statements).await {
                    return;
                }
            }
            _ = slow_ticker.tick() => {
                if paused { continue; }
                if !send_slow(&client, &tx).await {
                    return;
                }
            }
            _ = refresh_now.notified() => {
                if !poll_fast_with_reconnect(&mut client, &dsn, &tls, &tx, &mut has_pg_stat_statements, pg17_plus).await {
                    return;
                }
            }
            Some(ctrl) = control_rx.recv() => {
                match ctrl {
                    PollControl::SwitchDb(new_dbname) => {
                        if let Some(base) = &conn_base {
                            let new_dsn = build_conninfo(
                                &base.host,
                                base.port,
                                &base.user,
                                &new_dbname,
                                base.password.as_deref(),
                            );
                            match connect(&new_dsn, &tls).await {
                                Ok(new_client) => {
                                    client = new_client;
                                    dsn = new_dsn;
                                    has_pg_stat_statements = statements::extension_loaded(&client).await.unwrap_or(false);
                                    if tx.send(AppEvent::DbSwitched(new_dbname)).await.is_err() {
                                        return;
                                    }
                                    if let Ok(info) = serverinfo::fetch(&client).await {
                                        pg17_plus = info.version_num >= 170_000;
                                        if tx.send(AppEvent::ServerInfo(info)).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                                Err(e) => {
                                    if tx
                                        .send(AppEvent::Error {
                                            source: "switch database".to_string(),
                                            message: format!("{e:#}"),
                                        })
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    PollControl::SetInterval(d) => {
                        fast_ticker = tokio::time::interval(d);
                        fast_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                    }
                    PollControl::TogglePause => paused = !paused,
                    PollControl::Cancel(pid) => {
                        if !run_signal(&client, &tx, "pg_cancel_backend", pid).await {
                            return;
                        }
                    }
                    PollControl::Terminate(pid) => {
                        if !run_signal(&client, &tx, "pg_terminate_backend", pid).await {
                            return;
                        }
                    }
                }
            }
        }
    }
}
