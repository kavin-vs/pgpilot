# PgPilot

A lightweight, interactive terminal dashboard for Postgres — an overview with a built-in diagnosis, per-query stats (`pg_stat_statements`), live session activity with a blocking tree and cancel/terminate, buffer cache & I/O, and table/index health — in the spirit of `htop`/`k9s`, but for Postgres. Connects directly via a native Rust driver (`tokio-postgres`); does not shell out to the `psql` binary.

## Install / Build

Requires a Rust toolchain (`cargo`).

```
cargo build --release
```

## Development / Local Testing

Run straight from source with `cargo run` — no need to build a release binary while iterating. Anything after `--` is passed through to the app:

```
cargo run                          # bare invocation: saved-connection picker / wizard
cargo run -- --dbname postgres     # flag-driven, connects straight in, no prompts
cargo run -- --profile local       # connect using a profile you've already saved
```

This needs a real Postgres to talk to. If you don't have one running, the fastest local option on macOS is Homebrew:

```
brew install postgresql@14
brew services start postgresql@14
createdb $(whoami)                 # or any dbname you'll pass with --dbname
```

Other useful commands while developing:

```
cargo check          # fast type-check, no codegen
cargo clippy          # lint
cargo build --release # optimized binary at target/release/pgpilot
```

Quitting the TUI (`q`) always restores your terminal, even on a crash (`ratatui::init()` installs a panic hook for this) — so it's safe to Ctrl-C out of `cargo run` too if something hangs.

If you use [Claude Code](https://claude.com/claude-code), this repo ships three skills under `.claude/skills/` that encode this project's own workflows — `add-panel` (the checklist for wiring a new dashboard data source/tab), `debug-pgpilot` (known failure patterns: PG version-skew queries, terminal corruption from stray output, TLS build deps), and `release-pgpilot` (cutting a version bump). They trigger automatically on matching requests; no setup needed.

## Usage

```
pgpilot [OPTIONS]
```

### Connecting

**Run `pgpilot` with no flags** and it acts like `rclone`'s remote picker: if you have saved connections, it lists them for you to pick from (or add a new one); if you have none yet, it walks you straight into a short wizard (name, host, port, user, database) and saves the result for next time.

```
$ pgpilot
Saved connections:
  1) prod  (prod.db.internal:5432/app) [SSL]
  2) local (localhost:5432/postgres)
  e) Edit a connection
  n) Add new connection
  q) Quit
>
```

Saved profiles store host/port/user/dbname/SSL settings only — **never the password**. Password comes from `PGPASSWORD` if set, otherwise you'll get a masked `Password:` prompt (empty input means "no password needed," e.g. trust/peer auth). Profiles live in `~/Library/Application Support/pgpilot/config.toml` on macOS (via the `directories` crate; XDG-style paths on Linux), permissioned `0600`.

- `--profile <name>` connects directly using a saved profile by name, skipping the picker (still prompts for password if `PGPASSWORD` isn't set).
- `e` at the picker, then a connection number, re-opens the wizard for that connection, pre-filled with its current values — blank input keeps a field as-is, `-` clears an optional field (CA/client cert/key paths). Saves and connects when done.

**SSL**: when adding or editing a connection, answering `y` to "Use SSL?" prompts for three optional PEM paths, each independently skippable:
- **CA root cert** — verify the server's certificate against this CA. If left unset, the connection is still encrypted but the server's certificate isn't verified (equivalent to `sslmode=require`, not `verify-full`).
- **Client cert** + **client key** — for mutual TLS, if your server requires a client certificate. Both must be set together, or neither.

The same options are available non-interactively via `--ssl`, `--ssl-root-cert`, `--ssl-client-cert`, and `--ssl-client-key` (only apply to `--host`/`--user`/etc.-style flags, not `--dsn` — see below).

For scripting/CI, or if you'd rather not use saved profiles at all, the original flag-driven usage still works exactly as before and never prompts for anything:

1. `--dsn <connection-string>` — full override, e.g. `--dsn "postgres://user:pass@host:5432/dbname?sslmode=prefer"`. Takes precedence over everything below. Note: `--ssl*` flags don't apply to `--dsn` — encode SSL params in the DSN string itself if you need them here (`sslmode=`, `sslrootcert=`, etc. are libpq-native).
2. Otherwise built from individual flags, each with an env var fallback:
   - `--host` / `PGHOST` (default `localhost`)
   - `--port` / `PGPORT` (default `5432`)
   - `--user` / `PGUSER` (default: `$USER`)
   - `--dbname` / `PGDATABASE` (default: same as user)
   - `--ssl`, `--ssl-root-cert <path>`, `--ssl-client-cert <path>`, `--ssl-client-key <path>` — same semantics as the wizard's SSL prompts above.
3. `PGPASSWORD` is read directly from the environment (not a `--password` flag, so it never shows up in shell history or `--help` output).

Giving any of `--dsn`/`--host`/`--port`/`--user`/`--dbname` (as a flag or via its env var) bypasses the picker/wizard entirely — only a fully bare `pgpilot` (or `--profile`) goes interactive.

`--interval <secs>` sets the fast-tier poll interval (default `2`), independent of how the connection was resolved. Adjustable at runtime with `-`/`+` — see below.

`--ascii` swaps every Unicode block/braille glyph (sparklines, bars, cursors, the blocking-tree's connectors) for plain ASCII, for terminals/fonts that don't render them cleanly.

Examples:

```
pgpilot                    # picker/wizard
pgpilot --profile prod     # saved profile, no picker
pgpilot --dbname postgres  # flag-driven, no prompts
PGPASSWORD=secret pgpilot --dsn "postgres://user@host/db"
pgpilot --ascii            # plain-ASCII glyphs
```

### Tabs

| Key | Tab | What's in it |
|---|---|---|
| `1` | Overview | Stat cards for transactions/s and estimated p95 latency (both with sparklines) and a compact connections meter, the slowest statements, and a wait-event breakdown; below that, buffer cache hit ratio + sparkline, per-database cache hit, coldest relations (lowest cache hit), checkpoints & buffers, and replication stats. A tab-bar badge shows whether anything needs attention; `g` opens a full "diagnosis" popup ranking the top suspects behind current db load plus suggested fixes |
| `2` | Queries | `pg_stat_statements`-backed table (total/mean/stddev time, calls, disk I/O bytes, disk time, cache hit), sortable — including by disk I/O, which finds the query hammering storage even when its wall-clock time looks unremarkable — with a detail pane breaking out shared reads/writes, temp-file spill, and disk time for the selected statement. Disk time needs `track_io_timing = on` (off by default); the byte columns work regardless. Shows a clear "extension not loaded" notice instead of erroring if `pg_stat_statements` isn't installed — everything else in pgpilot works without it |
| `3` | Activity | Connection-state summary cards, the full `pg_stat_activity` list (selectable), a blocking tree, and a lock/transaction summary |
| `4` | Tables & Indexes | Schema size totals, a table list (dead-tuple %, xid age, seq-scans/hour, last autovacuum), unused/invalid indexes (with reclaimable size), and missing-index candidates (unindexed foreign keys, high seq-scan-ratio tables) |
| `5` | Triggers | Every user-defined trigger (`pg_trigger`, excluding internal foreign-key-backing ones) — schema, table, function, enabled/disabled state; `enter` on a selected row opens a full-screen popup with that trigger's function source (`pg_get_functiondef`) |

### Keys

| Key | Action |
|---|---|
| `1`–`5` | Switch tab |
| `↑`/`↓` or `j`/`k` | Scroll/select rows on the current tab (Queries, Activity, Tables & Indexes, Triggers) |
| `s` | Cycle sort (Queries: total time → mean time → calls → disk I/O; Tables & Indexes: size/name, press again to reverse) |
| `x` / `X` | Cancel / terminate the selected Activity row's backend (`pg_cancel_backend`/`pg_terminate_backend`) — real, immediate, no confirmation prompt, same spirit as `htop`'s kill. Requires the `pg_signal_backend` role (or superuser); otherwise the attempt fails with a status message, not a crash |
| `space` | Pause/resume polling |
| `-` / `+` | Slow down / speed up the fast-tier poll rate |
| `r` | Force an immediate refresh, without waiting for the next poll tick (works even while paused) |
| `d` | Open a full-screen database picker (owner, size, sessions, tps, cache hit, state); `enter` reconnects to the selected one (same host/user/SSL, just a different `dbname`), `esc`/`q`/`d` closes it without switching |
| `g` | Open the diagnosis popup (headline, ranked suspects, suggested fixes). `g`/`esc`/`q` closes it |
| `enter` | On the Triggers tab, with a row selected: open a full-screen popup showing that trigger's function source. `enter`/`esc`/`q` closes it |
| `e` | View the full text of the current error(s) — only active when the footer shows a red error, since the footer's single line truncates long Postgres error messages. `e`/`esc`/`q` closes it |
| `q` | Quit |

`d` only shows up (and works) when connected via a saved profile or `--host`/`--user`/etc. flags — a raw `--dsn` connection string can't be safely rewritten to point at a different database, so switching is unavailable in that mode.

If a query fails (a permission issue, a Postgres-version-specific column, etc.), only *that block* shows it — a red `✗` in its title and its error inline — while every other block on every other tab keeps updating normally, since each one is fetched and reported independently. If the connection itself drops, every block shows the error and the last-known-good data stays on screen (stale but visible) until the background poller reconnects automatically on the next tick. Switching databases works the same way under the hood — a fresh connection is opened before the old one is dropped, and every tab shows its loading spinner again until the first snapshot from the new database arrives.

Tested against Postgres 13 through 17 — Overview's checkpoint/buffer stats automatically use the right system view for the connected server's version (Postgres 17 moved those columns from `pg_stat_bgwriter` to a new `pg_stat_checkpointer` view).

Polling is tiered, not one-size-fits-all: counters/activity refresh at the `--interval`/`-`/`+`-controlled rate, `pg_stat_statements` every 15s, and catalog-heavy data (dead tuples, xid age, unindexed FKs, triggers) every 5 minutes — polling everything at sub-second rates would make the monitor itself a load problem.

A small spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) shows up in two places: on the plain terminal while the initial connection is being made, and inside any panel that's still waiting on its first query result.

## Scope

In: the five tabs above, the diagnosis popup, saved connection profiles (list/add/edit/pick, SSL/mutual-TLS), the database picker, cancel/terminate.

Out of scope (candidates for future versions): deleting a saved profile in place (listing, adding, and editing are all supported), true time-windowed wait-event profiling (current sampling is a cumulative-since-session-start count, not `pg_wait_sampling`-grade), and query-plan-derived index suggestions (missing-index candidates are limited to two mechanically-derivable heuristics — unindexed foreign keys and high seq-scan-ratio tables — not fabricated column-level `CREATE INDEX` guesses).
