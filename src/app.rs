use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use ratatui::widgets::TableState;

use crate::db::activity::ActivityData;
use crate::db::cache_io::{BgWriterStats, CacheOverall, ColdRelation, ReplicationRow};
use crate::db::connections::ConnectionsData;
use crate::db::databases::DatabaseRow;
use crate::db::indexes::{IndexRow, UnindexedForeignKey};
use crate::db::serverinfo::ServerInfo;
use crate::db::statements::StatementsData;
use crate::db::tables::TablesData;
use crate::db::triggers::TriggerRow;
use crate::event::StatusLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Overview,
    Queries,
    Activity,
    TablesIndexes,
    Triggers,
}

impl PanelKind {
    pub const ALL: [PanelKind; 5] = [
        PanelKind::Overview,
        PanelKind::Queries,
        PanelKind::Activity,
        PanelKind::TablesIndexes,
        PanelKind::Triggers,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            PanelKind::Overview => "Overview",
            PanelKind::Queries => "Queries",
            PanelKind::Activity => "Activity",
            PanelKind::TablesIndexes => "Tables & Indexes",
            PanelKind::Triggers => "Triggers",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    fn flipped(self) -> Self {
        match self {
            SortDirection::Asc => SortDirection::Desc,
            SortDirection::Desc => SortDirection::Asc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSortColumn {
    Size,
    Name,
}

impl TableSortColumn {
    fn next(self) -> Self {
        match self {
            TableSortColumn::Size => TableSortColumn::Name,
            TableSortColumn::Name => TableSortColumn::Size,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TableSortColumn::Size => "size",
            TableSortColumn::Name => "name",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueriesSortColumn {
    Total,
    Mean,
    Calls,
    Io,
}

impl QueriesSortColumn {
    fn next(self) -> Self {
        match self {
            QueriesSortColumn::Total => QueriesSortColumn::Mean,
            QueriesSortColumn::Mean => QueriesSortColumn::Calls,
            QueriesSortColumn::Calls => QueriesSortColumn::Io,
            QueriesSortColumn::Io => QueriesSortColumn::Total,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            QueriesSortColumn::Total => "total time",
            QueriesSortColumn::Mean => "mean time",
            QueriesSortColumn::Calls => "calls",
            QueriesSortColumn::Io => "disk I/O",
        }
    }
}

/// Recent-sample ring buffers behind the Overview tab's sparklines/deltas.
/// Each series is pushed on its own source tier's cadence (fast-tier metrics
/// sample often; `p95`, sourced from `pg_stat_statements`, only every medium
/// tick) — series naturally hold different numbers of samples over the same
/// wall-clock span, which is expected, not a bug.
#[derive(Debug, Default)]
pub struct History {
    pub tps: VecDeque<f64>,
    pub qps: VecDeque<f64>,
    pub conn: VecDeque<f64>,
    pub cache_pct: VecDeque<f64>,
    pub p95: VecDeque<f64>,
}

impl History {
    const CAPACITY: usize = 120;

    fn push(deque: &mut VecDeque<f64>, value: f64) {
        deque.push_back(value);
        if deque.len() > Self::CAPACITY {
            deque.pop_front();
        }
    }
}

/// Rate derived from two consecutive slow-tier `TablesData` snapshots,
/// keyed by `(schema_name, table_name)`. Kept separate from `TableRow`
/// itself so the db layer stays a pure fetch with no rate math.
#[derive(Debug, Clone, Copy, Default)]
pub struct TableRowRate {
    pub seq_scan_per_hour: Option<f64>,
}

pub struct App {
    pub active: PanelKind,

    pub connections: Option<ConnectionsData>,
    /// Overview's cache/checkpoint blocks, each independently fetched/failable
    /// (see `PanelSnapshot::source_label`) — `cache_overall` needs the `Instant`
    /// to derive commits/s and rollback% from consecutive-poll deltas.
    pub cache_overall: Option<(CacheOverall, Instant)>,
    pub cache_coldest: Option<Vec<ColdRelation>>,
    pub cache_checkpoints: Option<BgWriterStats>,
    pub cache_replication: Option<Vec<ReplicationRow>>,
    pub indexes: Option<Vec<IndexRow>>,
    pub unindexed_fks: Option<Vec<UnindexedForeignKey>>,
    pub tables: Option<(TablesData, Instant)>,
    pub tables_rates: HashMap<(String, String), TableRowRate>,
    pub databases: Option<(Vec<DatabaseRow>, Instant)>,
    pub databases_prev: Option<(Vec<DatabaseRow>, Instant)>,
    pub statements: Option<(StatementsData, Instant)>,
    pub activity: Option<ActivityData>,
    pub triggers: Option<Vec<TriggerRow>>,
    pub server_info: Option<ServerInfo>,

    pub history: History,
    /// One entry per fast-tier poll, holding the wait-event key (`"CPU /
    /// running"` when a backend is active but not blocked on anything
    /// tracked, else `"{wait_event_type}:{wait_event}"`) of every
    /// concurrently-active backend caught that poll — usually 0 or 1
    /// entries, occasionally more under real concurrency. Bounded to
    /// `History::CAPACITY` (a recent window, not cumulative-since-session,
    /// so old activity ages out) and — critically — a poll that caught
    /// *nothing* active still pushes an empty `Vec`, so the Overview chart
    /// can normalize percentages over every poll rather than only the
    /// active ones. That normalization is load-bearing: dividing by
    /// active-only samples made "CPU / running" read as ~100% on a database
    /// that was in fact idle nearly all the time (RDS's own CPU% agreed),
    /// since almost every poll that *did* catch something active caught a
    /// fast, cache-hot query with no wait event — a real production report.
    /// Point sampling still can't catch sub-poll-interval waits (a known
    /// ceiling shared with any `pg_stat_activity`-based approach short of
    /// `pg_wait_sampling`), but no longer overstates *how often* the
    /// database is doing anything at all.
    pub wait_event_samples: VecDeque<Vec<String>>,
    /// Most recent rollback share of the throughput chart's commit+rollback
    /// delta; not historized, just the latest value.
    pub rollback_pct: Option<f64>,
    /// Poll-to-poll delta of `pg_stat_database.temp_bytes` / dt — a rate,
    /// not the raw cumulative total (which only grows and can't tell you
    /// whether spilling is happening *now* vs. happened once weeks ago).
    /// `None` until the second poll, same as `rollback_pct`.
    pub temp_bytes_per_sec: Option<f64>,

    /// Sticky per-source errors (source label -> message) — a source's entry
    /// is cleared only when *that same source* succeeds again, not by any
    /// unrelated panel's success, since a poll cycle can now have some
    /// panels fail and others succeed independently (see `db::send_labeled`).
    pub errors: BTreeMap<String, String>,
    /// Toggled by `e` (only when `errors` is non-empty) to show the full,
    /// unwrapped text of every current error — the footer's single line
    /// truncates long Postgres error messages (DETAIL/HINT, etc.).
    pub error_detail_open: bool,
    /// Toggled by `g` to show the diagnosis modal (headline + ranked
    /// suspects + fix hints) — always openable, unlike `error_detail_open`,
    /// since the heuristic runs even with an empty result.
    pub diagnosis_open: bool,
    /// Transient status-line feedback (sort changed, refreshed, cancel/
    /// terminate result) — distinct from the sticky `errors` banner. The
    /// `Instant` lets the footer expire it back to the help text instead of
    /// sticking forever (see `set_status`/`widgets::STATUS_TTL`).
    pub status: Option<(String, StatusLevel, Instant)>,
    /// When the most recent snapshot was applied, for the "updated Xs ago" footer.
    pub last_refresh: Option<Instant>,
    pub should_quit: bool,
    /// Advanced once per render tick; drives the loading spinner's animation.
    pub spinner_frame: usize,

    pub current_dbname: Option<String>,
    /// `host:port`, for the header bar. `None` for a raw `--dsn` connection
    /// (an opaque string we don't parse apart — see `ConnParts`).
    pub server_addr: Option<String>,
    /// False when connected via a raw `--dsn` string, which can't be safely
    /// rebuilt against a different `dbname` — hides the 'd' popup/hint.
    pub can_switch_db: bool,
    /// `Some(selected index)` while the database picker is open.
    pub db_popup: Option<usize>,

    pub tables_state: TableState,
    pub tables_sort: TableSortColumn,
    pub tables_sort_dir: SortDirection,

    pub queries_state: TableState,
    pub queries_sort: QueriesSortColumn,

    pub activity_state: TableState,

    pub triggers_state: TableState,

    pub paused: bool,
    /// Current fast-tier poll interval — the only tier `-`/`+` adjust.
    /// Mirrors what's actually in effect in `poll_task`, updated locally on
    /// keypress for immediate header feedback rather than waiting on the
    /// control-channel round trip.
    pub rate: Duration,
    pub ascii: bool,
}

impl App {
    pub fn new(
        current_dbname: Option<String>,
        server_addr: Option<String>,
        can_switch_db: bool,
        ascii: bool,
        rate: Duration,
    ) -> Self {
        Self {
            active: PanelKind::Overview,
            connections: None,
            cache_overall: None,
            cache_coldest: None,
            cache_checkpoints: None,
            cache_replication: None,
            indexes: None,
            unindexed_fks: None,
            tables: None,
            tables_rates: HashMap::new(),
            databases: None,
            databases_prev: None,
            statements: None,
            activity: None,
            triggers: None,
            server_info: None,
            history: History::default(),
            wait_event_samples: VecDeque::new(),
            rollback_pct: None,
            temp_bytes_per_sec: None,
            errors: BTreeMap::new(),
            error_detail_open: false,
            diagnosis_open: false,
            status: None,
            last_refresh: None,
            should_quit: false,
            spinner_frame: 0,
            current_dbname,
            server_addr,
            can_switch_db,
            db_popup: None,
            tables_state: TableState::default(),
            tables_sort: TableSortColumn::Size,
            tables_sort_dir: SortDirection::Desc,
            queries_state: TableState::default(),
            queries_sort: QueriesSortColumn::Total,
            activity_state: TableState::default(),
            triggers_state: TableState::default(),
            paused: false,
            rate,
            ascii,
        }
    }

    fn active_row_count(&self) -> usize {
        match self.active {
            PanelKind::Queries => match &self.statements {
                Some((StatementsData::Available(rows), _)) => rows.len(),
                _ => 0,
            },
            PanelKind::Activity => self.activity.as_ref().map_or(0, |a| a.rows.len()),
            PanelKind::TablesIndexes => self.tables.as_ref().map_or(0, |(t, _)| t.tables.len()),
            PanelKind::Triggers => self.triggers.as_ref().map_or(0, |t| t.len()),
            _ => 0,
        }
    }

    pub fn scroll_down(&mut self) {
        let len = self.active_row_count();
        if len == 0 {
            return;
        }
        let state = match self.active {
            PanelKind::Queries => &mut self.queries_state,
            PanelKind::Activity => &mut self.activity_state,
            PanelKind::TablesIndexes => &mut self.tables_state,
            PanelKind::Triggers => &mut self.triggers_state,
            _ => return,
        };
        let next = state.selected().map_or(0, |i| (i + 1).min(len - 1));
        state.select(Some(next));
    }

    pub fn scroll_up(&mut self) {
        let len = self.active_row_count();
        if len == 0 {
            return;
        }
        let state = match self.active {
            PanelKind::Queries => &mut self.queries_state,
            PanelKind::Activity => &mut self.activity_state,
            PanelKind::TablesIndexes => &mut self.tables_state,
            PanelKind::Triggers => &mut self.triggers_state,
            _ => return,
        };
        let next = state.selected().map_or(0, |i| i.saturating_sub(1));
        state.select(Some(next));
    }

    /// Cycles the sort for the active panel, if it has one (Queries: total
    /// time -> mean time -> calls, always descending; TablesIndexes: same
    /// size/name toggle as before, reversing direction on repeat).
    pub fn cycle_sort(&mut self) {
        match self.active {
            PanelKind::Queries => {
                self.queries_sort = self.queries_sort.next();
                self.sort_statements();
                self.set_status(format!("sorted by {}", self.queries_sort.label()), StatusLevel::Info);
            }
            PanelKind::TablesIndexes => {
                let next = self.tables_sort.next();
                self.tables_sort_dir = if next == self.tables_sort {
                    self.tables_sort_dir.flipped()
                } else {
                    SortDirection::Desc
                };
                self.tables_sort = next;
                self.sort_tables();
            }
            _ => {}
        }
    }

    fn sort_tables(&mut self) {
        let Some((data, _)) = &mut self.tables else {
            return;
        };
        let asc = self.tables_sort_dir == SortDirection::Asc;
        data.tables.sort_by(|a, b| {
            let ord = match self.tables_sort {
                TableSortColumn::Size => a.total_bytes.cmp(&b.total_bytes),
                TableSortColumn::Name => a.table_name.cmp(&b.table_name),
            };
            if asc { ord } else { ord.reverse() }
        });
    }

    fn sort_statements(&mut self) {
        let Some((StatementsData::Available(rows), _)) = &mut self.statements else {
            return;
        };
        match self.queries_sort {
            QueriesSortColumn::Total => rows.sort_by(|a, b| b.total_exec_time_ms.total_cmp(&a.total_exec_time_ms)),
            QueriesSortColumn::Mean => rows.sort_by(|a, b| b.mean_exec_time_ms.total_cmp(&a.mean_exec_time_ms)),
            QueriesSortColumn::Calls => rows.sort_by_key(|r| std::cmp::Reverse(r.calls)),
            // ponytail: sorts on block counts, not `io_time_ms` — blocks are always
            // populated, timing is 0 unless `track_io_timing = on`. Switch the key to
            // time (falling back to blocks) if the timing-off case stops mattering.
            QueriesSortColumn::Io => rows.sort_by_key(|r| std::cmp::Reverse(r.io_blocks())),
        }
    }

    /// Opens the database picker, pre-selecting the currently connected
    /// database if it's in the list. No-op when switching isn't supported
    /// for this connection (raw `--dsn`).
    pub fn open_db_popup(&mut self) {
        if !self.can_switch_db {
            return;
        }
        let selected = self
            .databases
            .as_ref()
            .zip(self.current_dbname.as_deref())
            .and_then(|((dbs, _), cur)| dbs.iter().position(|d| d.name == cur))
            .unwrap_or(0);
        self.db_popup = Some(selected);
    }

    pub fn close_db_popup(&mut self) {
        self.db_popup = None;
    }

    pub fn popup_move(&mut self, delta: i32) {
        let (Some(selected), Some(len)) = (self.db_popup, self.databases.as_ref().map(|(d, _)| d.len())) else {
            return;
        };
        if len == 0 {
            return;
        }
        let next = if delta < 0 {
            selected.saturating_sub(1)
        } else {
            (selected + 1).min(len - 1)
        };
        self.db_popup = Some(next);
    }

    pub fn popup_selected_database(&self) -> Option<&DatabaseRow> {
        let selected = self.db_popup?;
        self.databases.as_ref()?.0.get(selected)
    }

    pub fn set_status(&mut self, text: impl Into<String>, level: StatusLevel) {
        self.status = Some((text.into(), level, Instant::now()));
    }

    pub fn selected_activity_pid(&self) -> Option<i32> {
        let idx = self.activity_state.selected()?;
        self.activity.as_ref()?.rows.get(idx).map(|r| r.pid)
    }

    pub fn record_connections(&mut self, data: ConnectionsData) {
        History::push(&mut self.history.conn, data.used as f64);
        self.connections = Some(data);
    }

    pub fn record_cache_overall(&mut self, data: CacheOverall, now: Instant) {
        if let Some((old, old_at)) = &self.cache_overall {
            let dt = now.duration_since(*old_at).as_secs_f64();
            if dt > 0.0 {
                let commit_delta = (data.xact_commit - old.xact_commit) as f64;
                let rollback_delta = (data.xact_rollback - old.xact_rollback) as f64;
                let tps = ((commit_delta + rollback_delta) / dt).max(0.0);
                History::push(&mut self.history.tps, tps);

                let total = commit_delta + rollback_delta;
                self.rollback_pct = if total > 0.0 {
                    Some((rollback_delta / total * 100.0).clamp(0.0, 100.0))
                } else {
                    None
                };

                let temp_bytes_delta = (data.temp_bytes - old.temp_bytes).max(0) as f64;
                self.temp_bytes_per_sec = Some(temp_bytes_delta / dt);
            }
        }
        History::push(&mut self.history.cache_pct, data.hit_ratio_pct.unwrap_or(0.0));
        self.cache_overall = Some((data, now));
    }

    pub fn record_tables(&mut self, data: TablesData, now: Instant) {
        if let Some((old, old_at)) = &self.tables {
            let dt = now.duration_since(*old_at).as_secs_f64();
            if dt > 0.0 {
                let mut rates = HashMap::new();
                for t in &data.tables {
                    if let Some(prev) = old
                        .tables
                        .iter()
                        .find(|p| p.schema_name == t.schema_name && p.table_name == t.table_name)
                    {
                        let per_sec = (t.seq_scan - prev.seq_scan) as f64 / dt;
                        rates.insert(
                            (t.schema_name.clone(), t.table_name.clone()),
                            TableRowRate {
                                seq_scan_per_hour: Some((per_sec * 3600.0).max(0.0)),
                            },
                        );
                    }
                }
                self.tables_rates = rates;
            }
        }
        self.tables = Some((data, now));
        self.sort_tables();
    }

    pub fn record_databases(&mut self, data: Vec<DatabaseRow>, now: Instant) {
        self.databases_prev = self.databases.take();
        self.databases = Some((data, now));
    }

    /// Queries/s (from `pg_stat_statements` call-count deltas) and a
    /// call-weighted `mean + 2*stddev` p95 estimate — `pg_stat_statements`
    /// has no true percentile, this approximates one assuming a roughly
    /// normal per-query latency spread. A known ceiling, not exact.
    pub fn record_statements(&mut self, data: StatementsData, now: Instant) {
        if let StatementsData::Available(rows) = &data {
            if let Some((StatementsData::Available(old_rows), old_at)) = &self.statements {
                let dt = now.duration_since(*old_at).as_secs_f64();
                if dt > 0.0 {
                    let new_calls: i64 = rows.iter().map(|r| r.calls).sum();
                    let old_calls: i64 = old_rows.iter().map(|r| r.calls).sum();
                    let qps = ((new_calls - old_calls) as f64 / dt).max(0.0);
                    History::push(&mut self.history.qps, qps);
                }
            }

            let total_calls: i64 = rows.iter().map(|r| r.calls).sum();
            if total_calls > 0 {
                let weighted: f64 = rows
                    .iter()
                    .map(|r| (r.mean_exec_time_ms + 2.0 * r.stddev_exec_time_ms) * r.calls as f64)
                    .sum();
                History::push(&mut self.history.p95, weighted / total_calls as f64);
            }
        }
        self.statements = Some((data, now));
        self.sort_statements();
    }

    pub fn record_activity(&mut self, data: ActivityData) {
        let sample: Vec<String> = data
            .rows
            .iter()
            .filter(|row| row.state.as_deref() == Some("active"))
            .map(|row| match (&row.wait_event_type, &row.wait_event) {
                (Some(t), Some(e)) => format!("{t}:{e}"),
                _ => "CPU / running".to_string(),
            })
            .collect();
        self.wait_event_samples.push_back(sample);
        if self.wait_event_samples.len() > History::CAPACITY {
            self.wait_event_samples.pop_front();
        }
        self.activity = Some(data);
    }

    /// Resets all per-database panel state after a successful 'd'-popup
    /// database switch; cluster-wide state (the database list itself) is
    /// left alone since it doesn't depend on which database is current.
    pub fn on_db_switched(&mut self, name: String) {
        self.current_dbname = Some(name);
        self.connections = None;
        self.cache_overall = None;
        self.cache_coldest = None;
        self.cache_checkpoints = None;
        self.cache_replication = None;
        self.indexes = None;
        self.unindexed_fks = None;
        self.tables = None;
        self.tables_rates.clear();
        self.statements = None;
        self.activity = None;
        self.triggers = None;
        self.server_info = None;
        self.history = History::default();
        self.wait_event_samples.clear();
        self.rollback_pct = None;
        self.temp_bytes_per_sec = None;
        self.errors.clear();
        self.error_detail_open = false;
        self.diagnosis_open = false;
        self.tables_state = TableState::default();
        self.queries_state = TableState::default();
        self.activity_state = TableState::default();
        self.triggers_state = TableState::default();
    }
}
