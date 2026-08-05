# Real-world Postgres incidents pgpilot can detect

Research pass behind the `v6` diagnosis expansion (see `CLAUDE.md`). Scoped down from a generic
"top 100 Postgres problems" sweep to: real, independently-documented production incident classes
that a live monitoring TUI can plausibly catch — i.e. detectable from data pgpilot already polls
(`pg_stat_activity`, `pg_stat_user_tables`, `pg_stat_bgwriter`/`pg_stat_checkpointer`,
`pg_stat_database`, `pg_stat_replication`, `pg_stat_statements`), not config-tuning advice that
requires a human to act outside the dashboard.

For each: the problem, why it's a real recurring failure (not theoretical), pgpilot's coverage
before this change, and the fix. Ranked roughly by blast radius.

---

## 1. XID (transaction ID) wraparound

**Problem.** Postgres XIDs are 32-bit. Once a table's `age(relfrozenxid)` gets close to the
wraparound point (~2^31, with autovacuum's own hard-stop before that), Postgres refuses to assign
new XIDs — every write on the cluster fails until an operator runs `VACUUM` under time pressure,
often in single-user mode. There is no gradual latency warning beforehand; the system runs
normally until it doesn't.

**Real incidents.** Documented at Sentry and at Mailchimp's Mandrill (a spike in writes on one of
five physical Postgres instances triggered forced wraparound protection). Percona and Netdata both
run dedicated "how to survive this" guides because it recurs across the industry.

**Thresholds (Netdata's guide).** `age(datfrozenxid) > 500,000,000` → autovacuum is falling behind
the freeze pace. `> 1,000,000,000` → urgent, freeze work must complete soon. By ~40M XIDs remaining
before the hard limit, Postgres is already emitting WARNING log lines.

**pgpilot coverage before.** `TableRow.xid_age` (`age(relfrozenxid)`) was already fetched by
`db/tables.rs` and shown as plain text inside the existing "bloat" suspect — never thresholded or
ranked on its own.

**Fix.** New `diagnose()` candidate scanning `tables` for the worst `xid_age`, `Warn` at 500M,
`Bad` at 1B — scored high enough to consistently outrank cosmetic suspects, matching the severity
of what it represents.

Sources: [Netdata — Postgres transaction ID wraparound](https://www.netdata.cloud/guides/postgres/postgres-transaction-id-wraparound/),
[Percona — Overcoming VACUUM Wraparound](https://www.percona.com/blog/overcoming-vacuum-wraparound/)

---

## 2. Connection pool exhaustion

**Problem.** `FATAL: too many clients already` / `FATAL: sorry, too many clients already`. Once
`pg_stat_activity` fills up to `max_connections`, every new connection attempt fails outright —
including the app's own health checks and the on-call engineer's own `psql`.

**Real incidents.** A documented RCA: an analytics-pipeline service's 117-connection pool exhausted
under a marketing-campaign traffic spike, 128 minutes of downtime, 2,568 users and 4 dependent
services affected. A separate write-up describes PgBouncer hitting "too many clients" at 2am and
the app-pool debugging that followed becoming a standing incident playbook.

**pgpilot coverage before.** `ConnectionsData{used, max_connections}` was already fetched every
fast-tier poll and shown as an Overview meter — never wired into `DiagnosisInputs` at all, so it
never triggered a suspect or alert no matter how close to the ceiling it got.

**Fix.** New candidate: `used / max_connections`, `Warn` at 80%, `Bad` at 90% — standard
capacity-planning headroom, giving warning laps before the hard "too many clients" wall.

Sources: [Connection pool exhaustion RCA](https://glama.ai/mcp/servers/@claudiogarza/obsidian-rag-mcp/blob/3d975081cda40e0bb7ec3722db0ae588c2519258/vault/RCAs/2025-02-24-database-connection-pool-exhaustion.md),
[PgBouncer "too many clients" at 2am](https://medium.com/@developeryusuf/pgbouncer-hit-too-many-clients-at-2-am-340c6bc8bf5c)

---

## 3. Checkpoint storms

**Problem.** Bulk writes (batch jobs, large migrations) force checkpoints ahead of schedule. Every
other query on the instance sees a p99 spike while the storage queue drains the checkpoint's dirty
pages, and the bulk operation itself competes with its own checkpoint I/O.

**Detection (Netdata / pgedge).** `checkpoints_req` more than ~10% of `checkpoints_timed` means
`max_wal_size` is too small for the actual WAL generation rate — checkpoints are being forced
rather than landing on their timed schedule. Separately, `maxwritten_clean > 0` (from
`pg_stat_bgwriter`) means the background writer isn't keeping up and backend processes are picking
up the write slack themselves, which is strictly worse for latency.

**pgpilot coverage before.** `BgWriterStats` was already fetched and these exact two booleans
already drive the Overview UI's color-coding on the "Checkpoints & Buffers" block (since v4) — but
were never fed into `diagnosis.rs`, so a real checkpoint-storm pattern never surfaced as a suspect
or alert, only as a color a user had to notice on their own.

**Fix.** New candidate reusing those same two booleans: `Bad` when checkpoints are mostly forced,
`Warn` when the bgwriter is falling behind.

Sources: [Netdata — Postgres checkpoint storms](https://www.netdata.cloud/guides/postgres/postgres-checkpoint-storms/),
[pgEdge — Checkpoints, Write Storms, and You](https://www.pgedge.com/blog/checkpoints-write-storms-and-you)

---

## 4. work_mem / temp-file disk spill

**Problem.** Sorts, hash joins, and hash aggregates that don't fit in `work_mem` spill to disk as
temp files. This is one of the most common "tiny config change → big win" stories in Postgres
operations — a `work_mem` bump (or fixing the query driving the spill) can eliminate disk I/O that
was invisible until someone looked at `pg_stat_database.temp_bytes`.

**Real incident.** "work_mem: it's a trap!" documents a production cluster that hit this
unexpectedly, walking through `pg_stat_activity`'s memory-context diagnostics to find it.

**pgpilot coverage before.** `CacheOverall.temp_files`/`temp_bytes` (from `pg_stat_database`) were
already fetched every fast-tier poll and shown nowhere except raw numbers in a stat card —
never thresholded.

**Fix, v6→v8 correction.** v6 shipped a candidate thresholding the raw cumulative `temp_bytes`
(`Warn` at 1 GiB, `Bad` at 10 GiB) with a known-ceiling comment flagging it as "not a live rate."
That ceiling turned out to be a real production report within the same session: a user saw "temp
files: 790,078 (2.9 TB)" and asked what it was based on — the honest answer was "the lifetime total
since the database's stats were last reset, which could be weeks of history, not anything
necessarily happening right now." v7 replaced the threshold with a poll-to-poll delta
(`App::temp_bytes_per_sec`, computed in `record_cache_overall` the same way `tps`/`rollback_pct`
already are), so the suspect/alert now reads "spilling N MB/s right now" — `Warn` at 1 MiB/s, `Bad`
at 10 MiB/s — instead of flagging old, unrelated history as an ongoing problem. The raw cumulative
total is still shown in the Buffer Cache block for context, now explicitly labeled "since last
stats reset."

Sources: [work_mem: it's a trap!](https://mydbanotebook.org/posts/work_mem-its-a-trap/),
[KloudDB — Temporary files in PostgreSQL](https://klouddb.io/temporary-files-in-postgresql-steps-to-identify-and-fix-temp-file-issues/)

---

## 5. Replication lag

**Problem.** The common replication failure mode isn't a hard outage, it's silent staleness — "the
replica is 6 minutes behind" quietly breaks read-after-write assumptions across an app until
someone notices stale reads.

**Cited thresholds.** `replay_lag > 30s` sustained → warn; `> 5 minutes` → crosses most
application read-freshness assumptions and needs attention.

**pgpilot coverage before.** `fetch_replication` already queried `pg_stat_replication` for
`application_name` and byte lag (`pg_wal_lsn_diff`) — the far more actionable *time*-based lag
columns (`replay_lag` etc.) live on the exact same view and were simply never selected.

**Fix.** Add `replay_lag_secs` (`EXTRACT(EPOCH FROM replay_lag)`) to the existing query — no new
query, one more column — then threshold it: `Warn` at 30s, `Bad` at 300s.

Sources: [Monitor PostgreSQL Replication Lag with pg_stat_replication](https://www.postgresscripts.com/post/monitor-postgresql-replication-lag-with-pg-stat-replication/),
[How to Monitor PostgreSQL Replication Lag](https://oneuptime.com/blog/post/2026-01-21-postgresql-replication-lag-monitoring/view)

---

## 6. Lock wait chains

**Problem.** One long lock holder can stall a growing chain of waiters behind it; past a certain
chain depth or wait duration this becomes a user-facing outage, not just slow queries.
`pg_blocking_pids()` (built into Postgres since 9.6) is the standard way to walk the chain without
a hand-rolled `pg_locks` self-join.

**pgpilot coverage before.** `ActivityRow.blocked_by` (via `pg_blocking_pids(pid)`) was already
fetched every fast-tier poll and feeds the Activity tab's one-level blocking tree — never scored as
a suspect or alert, so a real pile-up was visible only to someone already looking at that tab.

**Fix.** New candidate: activity rows with a non-empty `blocked_by` and `duration_secs` above 10s.
Same one-level-deep ceiling as the Activity tab's existing blocking tree (not a full recursive
chain) — consistent limitation, not a new one.

Sources: [pganalyze — Lock monitoring in Postgres](https://pganalyze.com/blog/postgres-lock-monitoring),
[postgres.ai — Useful queries to analyze PostgreSQL lock trees](https://postgres.ai/blog/20211018-postgresql-lock-trees)

---

## 7. N+1 query signature

**Problem.** A parent query followed by many near-identical child queries in a loop (classic
ORM N+1) instead of one JOIN or batch call. Individually each call is fast, so it never shows up as
a "slow query" — the damage is in the round-trip count.

**Detection signature (multiple independent sources agree).** In `pg_stat_statements`: high
`calls`, low `mean_exec_time_ms`, and close to 1 row returned per call. A query called 10,000 times
a minute returning one row each time is very likely a loop, not a legitimate hot query.

**pgpilot coverage before.** `calls`, `rows`, and `mean_exec_time_ms` were already fetched per
statement for the Queries tab table — never pattern-matched against this signature.

**Fix.** New **alert-only** check (not a ranked suspect — this is the fuzziest heuristic of the
seven, matching how `diagnosis.rs` already treats other soft signals like unused indexes): `calls
>= 500`, `rows/calls <= 1.5`, `mean_exec_time_ms <= 5.0`.

Sources: pattern confirmed independently across `pg_stat_statements` query-analysis guides
(Tiger Data, virtual-dba.com, MonPG) during this research pass.

---

## What this doesn't cover

Deliberately out of scope for this pass — either not detectable from data pgpilot already polls,
or genuinely a human/schema decision rather than a monitoring signal:

- Index/query design advice requiring `EXPLAIN` plan analysis (missing composite indexes, join
  order) — `ui/tables_indexes.rs`'s `missing_index_candidates()` already documents this exact
  ceiling for the same reason.
- `shared_buffers`/`effective_cache_size`/`autovacuum_vacuum_scale_factor` tuning — these are
  config changes an operator makes, not something a dashboard "detects"; pgpilot's alerts already
  point at the right knob (e.g. the existing dead-tuple-% alert's `autovacuum_vacuum_scale_factor`
  hint) without pretending to compute the right value for an unknown workload.
- Partitioning strategy, backup/failover testing, extension recommendations — operational
  practices, not point-in-time signals.
