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
use crate::db::cache_io::CacheOverall;
use crate::db::indexes::{IndexRow, UnindexedForeignKey};
use crate::db::statements::StatementsData;
use crate::db::tables::TablesData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

const IDLE_IN_TXN_THRESHOLD_SECS: f64 = 60.0;
const DEAD_TUPLE_PCT_THRESHOLD: f64 = 10.0;
const SEQ_SCAN_HEAVY_THRESHOLD: i64 = 100;
const IDX_USE_LOW_PCT: f64 = 50.0;

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
            hint: "below the usual >=99% target — see Cache & I/O".to_string(),
        });
    }

    alerts.truncate(5);
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
}
