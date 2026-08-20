use crate::db::activity::ActivityData;
use crate::db::cache_io::{BgWriterStats, CacheOverall, ColdRelation, ReplicationRow};
use crate::db::connections::ConnectionsData;
use crate::db::databases::DatabaseRow;
use crate::db::indexes::{IndexRow, UnindexedForeignKey};
use crate::db::playground::StatementResult;
use crate::db::serverinfo::ServerInfo;
use crate::db::statements::StatementsData;
use crate::db::tables::TablesData;
use crate::db::triggers::TriggerRow;

/// One variant per independently-fetched, independently-failable block —
/// the cache/checkpoint blocks in particular used to be one bundled fetch
/// where any single query failing blanked out the other three; splitting
/// them (matching each block rendered on Overview) means every block shows
/// its own error state instead.
pub enum PanelSnapshot {
    Connections(ConnectionsData),
    CacheOverall(CacheOverall),
    CacheColdest(Vec<ColdRelation>),
    CacheCheckpoints(BgWriterStats),
    CacheReplication(Vec<ReplicationRow>),
    Indexes(Vec<IndexRow>),
    UnindexedForeignKeys(Vec<UnindexedForeignKey>),
    Tables(TablesData),
    Databases(Vec<DatabaseRow>),
    Statements(StatementsData),
    Activity(ActivityData),
    Triggers(Vec<TriggerRow>),
}

impl PanelSnapshot {
    /// Matches the `label` `db::send_labeled` used for this panel's fetch,
    /// so a successful snapshot clears exactly that source's sticky error
    /// (`App::errors`) rather than every source's, and so `ui::widgets::
    /// loading_or_error` can look up the right block's error by the same key.
    pub fn source_label(&self) -> &'static str {
        match self {
            PanelSnapshot::Connections(_) => "connections",
            PanelSnapshot::CacheOverall(_) => "cache overview",
            PanelSnapshot::CacheColdest(_) => "coldest relations",
            PanelSnapshot::CacheCheckpoints(_) => "checkpoints & wal",
            PanelSnapshot::CacheReplication(_) => "replication",
            PanelSnapshot::Indexes(_) => "indexes",
            PanelSnapshot::UnindexedForeignKeys(_) => "unindexed foreign keys",
            PanelSnapshot::Tables(_) => "tables",
            PanelSnapshot::Databases(_) => "databases",
            PanelSnapshot::Statements(_) => "pg_stat_statements",
            PanelSnapshot::Activity(_) => "activity",
            PanelSnapshot::Triggers(_) => "triggers",
        }
    }
}

pub enum AppEvent {
    Snapshot(PanelSnapshot),
    /// A single source's fetch/action failed — `source` matches
    /// `PanelSnapshot::source_label()` for polled panels, or an action name
    /// ("cancel"/"terminate") for one-off commands. Sticky per source (see
    /// `App::errors`) until that same source succeeds again, since a poll
    /// cycle can now have some panels fail and others succeed independently.
    Error { source: String, message: String },
    /// Sent after `poll_task` successfully reconnects to a different
    /// database (see the 'd' popup), carrying the new database's name.
    DbSwitched(String),
    /// One-shot, fetched at connect/reconnect/db-switch time — not part of
    /// the tiered poll cycle (see `db::serverinfo`).
    ServerInfo(ServerInfo),
    /// Transient status-line feedback for a completed action (sort changed,
    /// refreshed, cancel/terminate result) — distinct from the sticky
    /// `Error` banner.
    Status(String, StatusLevel),
    /// One-shot Playground query result (or a playground-connection-level
    /// problem, e.g. a failed db-switch reconnect) — deliberately not a
    /// `PanelSnapshot`/sticky `App::errors` entry: a one-shot user action,
    /// not a polled panel that can be "persistently broken." `has_more`
    /// is only ever `true` for a single-`SELECT` result the task chose to
    /// paginate (see `db::playground::is_single_paginatable_select`).
    PlaygroundResult { result: Result<Vec<StatementResult>, String>, has_more: bool },
    /// A follow-up page for a paginated Playground query, requested by
    /// scrolling to the bottom of the output pane (see
    /// `main.rs::handle_playground_key`'s `PageDown` arm). Always carries a
    /// `Result` (rather than a separate error path) so the UI can
    /// unconditionally clear `App::playground_fetching_more` from one match
    /// arm regardless of outcome — same reasoning as `PlaygroundResult`
    /// itself carrying a `Result`.
    PlaygroundMore { result: Result<StatementResult, String>, has_more: bool },
    /// The Playground connection's own backend pid (`pg_backend_pid()`),
    /// sent once at connect and again after every successful `SwitchDb`
    /// reconnect — lets Ctrl+C cancel a running Playground query via the
    /// existing `PollControl::Cancel` plumbing (see `main.rs::playground_cancel`).
    PlaygroundPid(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Warn,
}
