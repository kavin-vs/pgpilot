//! Pure, synchronous, no SQL of its own — derives the Overview tab's
//! "diagnosis" strip and "needs attention" alerts entirely from data other
//! tabs already fetched (`rust-build-notes.md`'s framing: "derived
//! in-process from the other views, free"). Takes explicit borrowed inputs
//! rather than `&App` so this module has no dependency on `App`'s shape.
//!
//! Ranking is a heuristic, not a true db-time accounting: candidate kinds are
//! scored on different, incomparable bases (a duration, a percentage, a call
//! count) and simply sorted by that score. Good enough to point at the right
//! haystack, not a precise cost model — a known ceiling.

use crate::db::activity::ActivityData;
use crate::db::cache_io::{BgWriterStats, CacheOverall, ReplicationRow};
use crate::db::connections::ConnectionsData;
use crate::db::indexes::{IndexRow, UnindexedForeignKey};
use crate::db::statements::StatementsData;
use crate::db::tables::TablesData;

/// `Ord` (Warn < Bad by declaration order) lets `alerts()` sort worst-first
/// before truncating, so a real high-severity alert can't get silently
/// starved behind lower-priority ones appended earlier in the function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warn,
    Bad,
}

#[derive(Debug, Clone)]
pub struct Suspect {
    pub rank: usize,
    pub kind: String,
    pub text: String,
    pub evidence: String,
    pub severity: Severity,
}

#[derive(Debug, Clone)]
pub struct Diagnosis {
    pub headline: String,
    pub suspects: Vec<Suspect>,
}

#[derive(Debug, Clone)]
pub struct Alert {
    pub severity: Severity,
    pub text: String,
    pub hint: String,
}

#[derive(Default)]
pub struct DiagnosisInputs<'a> {
    pub tables: Option<&'a TablesData>,
    pub indexes: Option<&'a [IndexRow]>,
    pub unindexed_fks: &'a [UnindexedForeignKey],
    pub activity: Option<&'a ActivityData>,
    pub statements: Option<&'a StatementsData>,
    pub cache: Option<&'a CacheOverall>,
    pub connections: Option<&'a ConnectionsData>,
    pub cache_checkpoints: Option<&'a BgWriterStats>,
    pub replication: Option<&'a [ReplicationRow]>,
    /// Poll-to-poll delta of `pg_stat_database.temp_bytes`, computed by
    /// `App::record_cache_overall` — not part of `CacheOverall` itself since
    /// that struct is a pure fetch with no derived state (same reason
    /// `App::rollback_pct` lives outside it too). A *rate*, not the raw
    /// cumulative-since-stats-reset total: a database that's been running
    /// for weeks can show terabytes of lifetime temp-file usage while
    /// currently spilling nothing at all, so thresholding the raw total
    /// (the original v6 approach) flagged old, unrelated activity as an
    /// ongoing problem.
    pub temp_bytes_per_sec: Option<f64>,
}

const IDLE_IN_TXN_THRESHOLD_SECS: f64 = 60.0;
const DEAD_TUPLE_PCT_THRESHOLD: f64 = 10.0;
const SEQ_SCAN_HEAVY_THRESHOLD: i64 = 100;
const IDX_USE_LOW_PCT: f64 = 50.0;

// See docs/postgres-incident-research.md for the real-incident evidence
// behind each threshold below.
/// Netdata's wraparound guide: >500M = autovacuum falling behind the freeze
/// pace, >1B = urgent — well ahead of the ~2^31 hard refusal point.
const XID_WRAPAROUND_WARN_AGE: i32 = 500_000_000;
const XID_WRAPAROUND_BAD_AGE: i32 = 1_000_000_000;
const CONN_PRESSURE_WARN_PCT: f64 = 80.0;
const CONN_PRESSURE_BAD_PCT: f64 = 90.0;
/// Sustained spill rate, not a lifetime total — see `DiagnosisInputs::temp_bytes_per_sec`.
/// A single fast-tier poll interval (default 2s) is a noisy window for a
/// rate, same tradeoff `record_cache_overall`'s existing tps computation
/// already accepts; not smoothed further.
const TEMP_SPILL_WARN_BYTES_PER_SEC: f64 = 1_048_576.0; // 1 MiB/s
const TEMP_SPILL_BAD_BYTES_PER_SEC: f64 = 10_485_760.0; // 10 MiB/s
const REPLICATION_LAG_WARN_SECS: f64 = 30.0;
const REPLICATION_LAG_BAD_SECS: f64 = 300.0;
/// One level deep, same ceiling as the Activity tab's own blocking tree.
const LOCK_WAIT_SECS: f64 = 10.0;
/// N+1 signature: many calls, near-1 row each, individually fast — a real
/// slow query never fits this shape. Heuristic, not a certain diagnosis.
const N1_CALLS_MIN: i64 = 500;
const N1_ROWS_PER_CALL_MAX: f64 = 1.5;
const N1_MEAN_MS_MAX: f64 = 5.0;

struct Candidate {
    score: f64,
    suspect: Suspect,
}

pub fn diagnose(inputs: &DiagnosisInputs) -> Diagnosis {
    let mut candidates: Vec<Candidate> = Vec::new();

    if let Some(StatementsData::Available(rows)) = inputs.statements
        && let Some(top) = rows.iter().max_by(|a, b| a.total_exec_time_ms.total_cmp(&b.total_exec_time_ms))
    {
        let total: f64 = rows.iter().map(|r| r.total_exec_time_ms).sum();
        let share = if total > 0.0 { top.total_exec_time_ms / total * 100.0 } else { 0.0 };
        candidates.push(Candidate {
            score: top.total_exec_time_ms,
            suspect: Suspect {
                rank: 0,
                kind: "slowest query".to_string(),
                text: truncate(&top.query, 90),
                evidence: format!("{:.0} ms mean x {} calls", top.mean_exec_time_ms, top.calls),
                severity: if share > 30.0 { Severity::Bad } else { Severity::Warn },
            },
        });
    }

    if let Some(activity) = inputs.activity
        && let Some(worst) = activity
            .rows
            .iter()
            .filter(|r| r.state.as_deref() == Some("idle in transaction"))
            .filter(|r| r.duration_secs.unwrap_or(0.0) >= IDLE_IN_TXN_THRESHOLD_SECS)
            .max_by(|a, b| a.duration_secs.unwrap_or(0.0).total_cmp(&b.duration_secs.unwrap_or(0.0)))
    {
        let dur = worst.duration_secs.unwrap_or(0.0);
        candidates.push(Candidate {
            score: dur,
            suspect: Suspect {
                rank: 0,
                kind: "idle in transaction".to_string(),
                text: format!("pid {} open {} — blocks other backends and stalls vacuum", worst.pid, crate::format::human_duration(dur)),
                evidence: "pg_stat_activity".to_string(),
                severity: Severity::Bad,
            },
        });
    }

    if let Some(tables) = inputs.tables
        && let Some(worst) = tables
            .tables
            .iter()
            .filter(|t| t.dead_tuple_pct.unwrap_or(0.0) >= DEAD_TUPLE_PCT_THRESHOLD)
            .max_by(|a, b| a.dead_tuple_pct.unwrap_or(0.0).total_cmp(&b.dead_tuple_pct.unwrap_or(0.0)))
    {
        let pct = worst.dead_tuple_pct.unwrap_or(0.0);
        candidates.push(Candidate {
            score: pct,
            suspect: Suspect {
                rank: 0,
                kind: "bloat / autovacuum behind".to_string(),
                text: format!("{}.{} — {:.1}% dead tuples, xid age {}", worst.schema_name, worst.table_name, pct, worst.xid_age),
                evidence: "pg_stat_user_tables".to_string(),
                severity: if pct >= 20.0 { Severity::Bad } else { Severity::Warn },
            },
        });
    }

    if let Some(fk) = inputs.unindexed_fks.first() {
        candidates.push(Candidate {
            score: 40.0,
            suspect: Suspect {
                rank: 0,
                kind: "unindexed foreign key".to_string(),
                text: format!("{}.{} ({}) — no covering index", fk.schema_name, fk.table_name, fk.columns),
                evidence: "pg_constraint".to_string(),
                severity: Severity::Warn,
            },
        });
    }

    if let Some(tables) = inputs.tables
        && let Some(worst) = tables
            .tables
            .iter()
            .filter(|t| t.seq_scan >= SEQ_SCAN_HEAVY_THRESHOLD && t.idx_use_pct.unwrap_or(100.0) < IDX_USE_LOW_PCT)
            .max_by_key(|t| t.seq_scan)
    {
        candidates.push(Candidate {
            score: worst.seq_scan as f64 * 0.1,
            suspect: Suspect {
                rank: 0,
                kind: "seq scans".to_string(),
                text: format!("{}.{} scanned {} times, {:.1}% index use", worst.schema_name, worst.table_name, worst.seq_scan, worst.idx_use_pct.unwrap_or(0.0)),
                evidence: "pg_stat_user_tables".to_string(),
                severity: Severity::Warn,
            },
        });
    }

    if let Some(tables) = inputs.tables
        && let Some(worst) = tables.tables.iter().max_by_key(|t| t.xid_age)
        && worst.xid_age >= XID_WRAPAROUND_WARN_AGE
    {
        candidates.push(Candidate {
            score: worst.xid_age as f64 / 1_000_000.0,
            suspect: Suspect {
                rank: 0,
                kind: "xid wraparound risk".to_string(),
                text: format!("{}.{} — xid age {} (of ~2.1B hard limit)", worst.schema_name, worst.table_name, worst.xid_age),
                evidence: "age(relfrozenxid)".to_string(),
                severity: if worst.xid_age >= XID_WRAPAROUND_BAD_AGE { Severity::Bad } else { Severity::Warn },
            },
        });
    }

    if let Some(conn) = inputs.connections
        && conn.max_connections > 0
    {
        let pct = conn.used as f64 / conn.max_connections as f64 * 100.0;
        if pct >= CONN_PRESSURE_WARN_PCT {
            candidates.push(Candidate {
                score: pct,
                suspect: Suspect {
                    rank: 0,
                    kind: "connection pressure".to_string(),
                    text: format!("{}/{} connections in use ({:.0}%)", conn.used, conn.max_connections, pct),
                    evidence: "pg_stat_activity".to_string(),
                    severity: if pct >= CONN_PRESSURE_BAD_PCT { Severity::Bad } else { Severity::Warn },
                },
            });
        }
    }

    if let Some(cp) = inputs.cache_checkpoints {
        let forced = cp.checkpoints_req > cp.checkpoints_timed;
        let backend_picking_up_slack = cp.maxwritten_clean > 0;
        if forced || backend_picking_up_slack {
            candidates.push(Candidate {
                score: if forced { 60.0 } else { 25.0 },
                suspect: Suspect {
                    rank: 0,
                    kind: "checkpoint storm".to_string(),
                    text: if forced {
                        format!("{} requested vs {} timed checkpoints — max_wal_size likely too small", cp.checkpoints_req, cp.checkpoints_timed)
                    } else {
                        format!("bgwriter falling behind — backends wrote {} time(s)", cp.maxwritten_clean)
                    },
                    evidence: "pg_stat_bgwriter / pg_stat_checkpointer".to_string(),
                    severity: if forced { Severity::Bad } else { Severity::Warn },
                },
            });
        }
    }

    if let Some(rate) = inputs.temp_bytes_per_sec
        && rate >= TEMP_SPILL_WARN_BYTES_PER_SEC
    {
        candidates.push(Candidate {
            score: rate / 1_048_576.0,
            suspect: Suspect {
                rank: 0,
                kind: "disk spill (work_mem)".to_string(),
                text: format!("spilling {}/s to temp files, sustained", crate::format::human_bytes(rate as i64)),
                evidence: "pg_stat_database (rate since last poll)".to_string(),
                severity: if rate >= TEMP_SPILL_BAD_BYTES_PER_SEC { Severity::Bad } else { Severity::Warn },
            },
        });
    }

    if let Some(replication) = inputs.replication
        && let Some(worst) = replication
            .iter()
            .filter(|r| r.replay_lag_secs.unwrap_or(0.0) >= REPLICATION_LAG_WARN_SECS)
            .max_by(|a, b| a.replay_lag_secs.unwrap_or(0.0).total_cmp(&b.replay_lag_secs.unwrap_or(0.0)))
    {
        let secs = worst.replay_lag_secs.unwrap_or(0.0);
        candidates.push(Candidate {
            score: secs,
            suspect: Suspect {
                rank: 0,
                kind: "replication lag".to_string(),
                text: format!("{} is {} behind", worst.application_name, crate::format::human_duration(secs)),
                evidence: "pg_stat_replication".to_string(),
                severity: if secs >= REPLICATION_LAG_BAD_SECS { Severity::Bad } else { Severity::Warn },
            },
        });
    }

    if let Some(activity) = inputs.activity
        && let Some(worst) = activity
            .rows
            .iter()
            .filter(|r| !r.blocked_by.is_empty() && r.duration_secs.unwrap_or(0.0) >= LOCK_WAIT_SECS)
            .max_by(|a, b| a.duration_secs.unwrap_or(0.0).total_cmp(&b.duration_secs.unwrap_or(0.0)))
    {
        let dur = worst.duration_secs.unwrap_or(0.0);
        let blocked_count = activity.rows.iter().filter(|r| !r.blocked_by.is_empty()).count();
        candidates.push(Candidate {
            score: dur,
            suspect: Suspect {
                rank: 0,
                kind: "lock wait chain".to_string(),
                text: format!(
                    "pid {} waiting {} on {:?} — {} backend(s) blocked",
                    worst.pid,
                    crate::format::human_duration(dur),
                    worst.blocked_by,
                    blocked_count
                ),
                evidence: "pg_blocking_pids()".to_string(),
                severity: Severity::Bad,
            },
        });
    }

    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let suspects: Vec<Suspect> = candidates
        .into_iter()
        .take(4)
        .enumerate()
        .map(|(i, c)| Suspect { rank: i + 1, ..c.suspect })
        .collect();

    let headline = if matches!(inputs.statements, Some(StatementsData::Available(_))) {
        if suspects.is_empty() {
            "No standout suspects — db time looks evenly spread.".to_string()
        } else {
            format!("Ranked {} suspect(s) by estimated impact.", suspects.len())
        }
    } else {
        "pg_stat_statements is not loaded, so per-query time is unavailable — ranking below \
         comes from live sessions and table statistics only."
            .to_string()
    };

    Diagnosis { headline, suspects }
}

pub fn alerts(inputs: &DiagnosisInputs) -> Vec<Alert> {
    let mut alerts = Vec::new();

    if let Some(activity) = inputs.activity {
        for row in activity
            .rows
            .iter()
            .filter(|r| r.state.as_deref() == Some("idle in transaction"))
            .filter(|r| r.duration_secs.unwrap_or(0.0) >= IDLE_IN_TXN_THRESHOLD_SECS)
        {
            alerts.push(Alert {
                severity: Severity::Bad,
                text: format!(
                    "pid {} idle in transaction for {}",
                    row.pid,
                    crate::format::human_duration(row.duration_secs.unwrap_or(0.0))
                ),
                hint: "holds locks and blocks vacuum until it commits or is terminated".to_string(),
            });
        }
    }

    if let Some(tables) = inputs.tables {
        for t in tables.tables.iter().filter(|t| t.dead_tuple_pct.unwrap_or(0.0) >= DEAD_TUPLE_PCT_THRESHOLD) {
            alerts.push(Alert {
                severity: Severity::Warn,
                text: format!("{}.{}: {:.1}% dead tuples", t.schema_name, t.table_name, t.dead_tuple_pct.unwrap_or(0.0)),
                hint: "consider a lower autovacuum_vacuum_scale_factor on this table".to_string(),
            });
        }
    }

    if let Some(rows) = inputs.indexes {
        let unused: Vec<&IndexRow> = rows.iter().filter(|i| i.idx_scan == 0).collect();
        if !unused.is_empty() {
            let reclaimable: i64 = unused.iter().map(|i| i.index_size_bytes).sum();
            alerts.push(Alert {
                severity: Severity::Warn,
                text: format!("{} unused index(es), {} reclaimable", unused.len(), crate::format::human_bytes(reclaimable)),
                hint: "zero scans since last stats reset — see Tables & Indexes".to_string(),
            });
        }
    }

    for fk in inputs.unindexed_fks {
        alerts.push(Alert {
            severity: Severity::Warn,
            text: format!("{}.{} ({}) — unindexed foreign key", fk.schema_name, fk.table_name, fk.columns),
            hint: format!("CREATE INDEX CONCURRENTLY ON {}.{} ({})", fk.schema_name, fk.table_name, fk.columns),
        });
    }

    if let Some(cache) = inputs.cache
        && cache.hit_ratio_pct.unwrap_or(100.0) < 95.0
    {
        alerts.push(Alert {
            severity: Severity::Warn,
            text: format!("buffer cache hit ratio {:.1}%", cache.hit_ratio_pct.unwrap_or(0.0)),
            hint: "below the usual >=99% target — see Overview".to_string(),
        });
    }

    if let Some(tables) = inputs.tables
        && let Some(worst) = tables.tables.iter().max_by_key(|t| t.xid_age)
        && worst.xid_age >= XID_WRAPAROUND_WARN_AGE
    {
        alerts.push(Alert {
            severity: if worst.xid_age >= XID_WRAPAROUND_BAD_AGE { Severity::Bad } else { Severity::Warn },
            text: format!("{}.{}: xid age {}", worst.schema_name, worst.table_name, worst.xid_age),
            hint: "run VACUUM (FREEZE) before hitting the ~2.1B wraparound limit and forced read-only mode".to_string(),
        });
    }

    if let Some(conn) = inputs.connections
        && conn.max_connections > 0
    {
        let pct = conn.used as f64 / conn.max_connections as f64 * 100.0;
        if pct >= CONN_PRESSURE_WARN_PCT {
            alerts.push(Alert {
                severity: if pct >= CONN_PRESSURE_BAD_PCT { Severity::Bad } else { Severity::Warn },
                text: format!("{:.0}% of max_connections in use ({}/{})", pct, conn.used, conn.max_connections),
                hint: "add pooling (e.g. PgBouncer) or raise max_connections before hitting 'too many clients already'".to_string(),
            });
        }
    }

    if let Some(cp) = inputs.cache_checkpoints {
        if cp.checkpoints_req > cp.checkpoints_timed {
            alerts.push(Alert {
                severity: Severity::Bad,
                text: format!("{} requested vs {} timed checkpoints", cp.checkpoints_req, cp.checkpoints_timed),
                hint: "raise max_wal_size so checkpoints hit their timeout instead of being forced".to_string(),
            });
        }
        if cp.maxwritten_clean > 0 {
            alerts.push(Alert {
                severity: Severity::Warn,
                text: format!("bgwriter fell behind {} time(s), backends picked up the writes", cp.maxwritten_clean),
                hint: "raise bgwriter_lru_maxpages".to_string(),
            });
        }
    }

    if let Some(rate) = inputs.temp_bytes_per_sec
        && rate >= TEMP_SPILL_WARN_BYTES_PER_SEC
    {
        alerts.push(Alert {
            severity: if rate >= TEMP_SPILL_BAD_BYTES_PER_SEC { Severity::Bad } else { Severity::Warn },
            text: format!("spilling {}/s to temp files right now", crate::format::human_bytes(rate as i64)),
            hint: "raise work_mem, or fix the sort/hash driving it — see Queries tab".to_string(),
        });
    }

    if let Some(replication) = inputs.replication {
        for r in replication.iter().filter(|r| r.replay_lag_secs.unwrap_or(0.0) >= REPLICATION_LAG_WARN_SECS) {
            let secs = r.replay_lag_secs.unwrap_or(0.0);
            alerts.push(Alert {
                severity: if secs >= REPLICATION_LAG_BAD_SECS { Severity::Bad } else { Severity::Warn },
                text: format!("{}: replaying {} behind", r.application_name, crate::format::human_duration(secs)),
                hint: "reads from this replica are stale by that much".to_string(),
            });
        }
    }

    if let Some(activity) = inputs.activity {
        for row in activity.rows.iter().filter(|r| !r.blocked_by.is_empty() && r.duration_secs.unwrap_or(0.0) >= LOCK_WAIT_SECS) {
            alerts.push(Alert {
                severity: Severity::Bad,
                text: format!(
                    "pid {} blocked {} on {:?}",
                    row.pid,
                    crate::format::human_duration(row.duration_secs.unwrap_or(0.0)),
                    row.blocked_by
                ),
                hint: "see Activity tab's blocking tree".to_string(),
            });
        }
    }

    if let Some(StatementsData::Available(rows)) = inputs.statements
        && let Some(worst) = rows
            .iter()
            .filter(|r| r.calls >= N1_CALLS_MIN && (r.rows as f64 / r.calls as f64) <= N1_ROWS_PER_CALL_MAX && r.mean_exec_time_ms <= N1_MEAN_MS_MAX)
            .max_by_key(|r| r.calls)
    {
        let rows_per_call = worst.rows as f64 / worst.calls as f64;
        alerts.push(Alert {
            severity: Severity::Warn,
            text: format!("{} — {} calls, ~{:.1} rows/call", truncate(&worst.query, 60), worst.calls, rows_per_call),
            hint: "looks like an N+1 query loop — batch into a single query or JOIN".to_string(),
        });
    }

    alerts.sort_by_key(|a| std::cmp::Reverse(a.severity));
    alerts.truncate(8);
    alerts
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tables::TableRow;

    #[test]
    fn high_seq_scan_low_cache_hit_names_that_suspect() {
        let tables = TablesData {
            tables: vec![TableRow {
                schema_name: "public".to_string(),
                table_name: "orders".to_string(),
                total_bytes: 1_000_000,
                indexes_toast_bytes: 200_000,
                live_tuples: 10_000,
                dead_tuple_pct: Some(1.0),
                xid_age: 1000,
                seq_scan: 5000,
                idx_use_pct: Some(0.2),
                last_vacuum_secs_ago: Some(60.0),
            }],
            schema_totals: Default::default(),
        };

        let inputs = DiagnosisInputs {
            tables: Some(&tables),
            ..Default::default()
        };

        let diagnosis = diagnose(&inputs);
        assert!(diagnosis.suspects.iter().any(|s| s.kind == "seq scans"));
    }

    #[test]
    fn empty_inputs_produce_no_suspects() {
        let inputs = DiagnosisInputs::default();
        let diagnosis = diagnose(&inputs);
        assert!(diagnosis.suspects.is_empty());
    }

    #[test]
    fn xid_age_past_bad_threshold_ranks_as_bad_wraparound_suspect() {
        let tables = TablesData {
            tables: vec![TableRow {
                schema_name: "public".to_string(),
                table_name: "events".to_string(),
                total_bytes: 1,
                indexes_toast_bytes: 0,
                live_tuples: 1,
                dead_tuple_pct: Some(0.0),
                xid_age: XID_WRAPAROUND_BAD_AGE + 1,
                seq_scan: 0,
                idx_use_pct: Some(100.0),
                last_vacuum_secs_ago: Some(1.0),
            }],
            schema_totals: Default::default(),
        };

        let inputs = DiagnosisInputs {
            tables: Some(&tables),
            ..Default::default()
        };

        let diagnosis = diagnose(&inputs);
        let wraparound = diagnosis.suspects.iter().find(|s| s.kind == "xid wraparound risk");
        assert!(matches!(wraparound, Some(s) if s.severity == Severity::Bad));
    }

    #[test]
    fn n_plus_one_signature_is_flagged_as_alert() {
        use crate::db::statements::StatementRow;

        let statements = StatementsData::Available(vec![StatementRow {
            query_id: 1,
            query: "SELECT * FROM widgets WHERE id = $1".to_string(),
            calls: 10_000,
            total_exec_time_ms: 5_000.0,
            mean_exec_time_ms: 0.5,
            stddev_exec_time_ms: 0.1,
            rows: 10_000,
            shared_blks_hit: 10_000,
            shared_blks_read: 0,
            shared_blks_written: 0,
            temp_blks_read: 0,
            temp_blks_written: 0,
            io_time_ms: 0.0,
            cache_hit_pct: Some(100.0),
        }]);

        let inputs = DiagnosisInputs {
            statements: Some(&statements),
            ..Default::default()
        };

        let found = alerts(&inputs).iter().any(|a| a.hint.contains("N+1"));
        assert!(found);
    }
}
