<div align="center">

# PgPilot

### `htop` / `k9s`, but for Postgres

A live, at-a-glance terminal dashboard for Postgres — connections, cache hit ratio,
blocking sessions, index health, and a ranked diagnosis of what's actually wrong — from
one binary that talks directly to the database over a native Rust driver.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macos%20%7C%20windows-blue?style=flat-square)](#9-platform-support)
[![Postgres](https://img.shields.io/badge/postgres-13%20--%2017-blue?style=flat-square)](#9-platform-support)

</div>

---

<div align="center">

[**Purpose**](#1-purpose) • [**Installation**](#2-installation) • [**Interactive Mode**](#3-interactive-mode-tabs) • [**Connecting**](#4-connecting) • [**Flags**](#5-flags--options) • [**Core Concept**](#6-core-concept)
<br>
[**Keys**](#7-keys) • [**Output Behavior**](#8-output-behavior) • [**Platform Support**](#9-platform-support) • [**Development**](#10-development) • [**Scope**](#11-scope)

</div>

---

## 1. Purpose

Something's wrong with a Postgres instance — connections are climbing, a query that used
to be instant now isn't, or a dashboard upstream just started paging. The honest way to
find out *why*, right now, is to open a `psql` shell and start querying
`pg_stat_activity`, `pg_stat_statements`, `pg_stat_bgwriter`, and half a dozen other
catalog views by hand, correlating the output yourself. That works. It's also the same
handful of queries, typed from memory, every single time — and the alternative, a full
Grafana/CloudWatch stack, is a project of its own to stand up just to get a five-minute
look.

**PgPilot** exists to close that gap. It connects directly to a Postgres instance and
continuously polls the same views that manual triage already reaches for — no exporter,
no agent, no dashboard to provision — and lays them out the way `htop` or `k9s` would:
one screen, refreshing live, organized around the questions an operator actually asks
(who's connected, what's slow, what's blocked, what's about to run out of headroom).

It goes one step further than a raw stat dump: since it's already polling everything,
PgPilot also scores what it sees against known incident patterns — xid wraparound risk,
checkpoint storms, lock chains, disk spill, replication lag — and surfaces a ranked
"here's what's probably wrong" list, derived entirely from data it was fetching anyway.

---

## 2. Installation

PgPilot has a quick install for macOS/Linux, a Chocolatey package for Windows, and a
manual download or build for everything else. No crates.io publish and no Homebrew tap
yet (deliberately deferred, see [Scope](#11-scope)) — this section only lists what
actually works, nothing aspirational.

### 2.1 Quick Install

<details>
<summary><strong>macOS / Linux</strong></summary>
<br>

```bash
curl -fsSL https://raw.githubusercontent.com/kavin-vs/pgpilot/master/install.sh | sh
```

Installs to `~/.local/bin` (override with `PGPILOT_INSTALL_DIR`). Make sure that
directory is on your `PATH`.
</details>

<details>
<summary><strong>Windows</strong></summary>
<br>

Via [Chocolatey](https://chocolatey.org):

```powershell
choco install pgpilot
```

(Until the package clears Chocolatey's moderation review, install the `.nupkg` attached
to a [release](https://github.com/kavin-vs/pgpilot/releases) directly:
`choco install pgpilot -s <folder containing the .nupkg>`.)

Or download the `.zip` for your platform from the
[Releases page](https://github.com/kavin-vs/pgpilot/releases) and unzip `pgpilot.exe`
somewhere on your `PATH`.
</details>

### 2.2 From Source

<details>
<summary><strong>cargo (any platform)</strong></summary>
<br>

Requires a Rust toolchain ([rustup.rs](https://rustup.rs)):

```bash
git clone https://github.com/kavin-vs/pgpilot.git
cd pgpilot
cargo build --release   # binary at target/release/pgpilot
```

Or skip the manual copy and let Cargo put it straight on your `PATH`:

```bash
cargo install --path .
```
</details>

### 2.3 Uninstalling

<details>
<summary><strong>Remove PgPilot</strong></summary>
<br>

If installed via `install.sh`:

```bash
rm "${PGPILOT_INSTALL_DIR:-$HOME/.local/bin}/pgpilot"
```

If installed via Chocolatey:

```powershell
choco uninstall pgpilot
```

If installed via `cargo install --path .`:

```bash
cargo uninstall pgpilot
```
</details>

---

## 3. Interactive Mode (Tabs)

Running `pgpilot` launches straight into the dashboard — there's no non-interactive
query mode, the TUI *is* the product. Six tabs, switched with `1`–`6`:

| Key | Tab | What's in it |
|---|---|---|
| `1` | Overview | Stat cards for transactions/s and estimated p95 latency (both with sparklines) and a compact connections meter, the slowest statements, and a wait-event breakdown; below that, buffer cache hit ratio + sparkline, per-database cache hit, coldest relations (lowest cache hit), checkpoints & buffers, and replication stats. A tab-bar badge shows whether anything needs attention; `g` opens a full "diagnosis" popup ranking the top suspects behind current db load plus suggested fixes |
| `2` | Queries | `pg_stat_statements`-backed table (total/mean/stddev time, calls, disk I/O bytes, disk time, cache hit), sortable — including by disk I/O, which finds the query hammering storage even when its wall-clock time looks unremarkable — with a detail pane breaking out shared reads/writes, temp-file spill, and disk time for the selected statement. Disk time needs `track_io_timing = on` (off by default); the byte columns work regardless. Shows a clear "extension not loaded" notice instead of erroring if `pg_stat_statements` isn't installed — everything else in pgpilot works without it |
| `3` | Activity | Connection-state summary cards, the full `pg_stat_activity` list (selectable) with an always-visible detail pane for the selected backend, a blocking tree, and a lock/transaction summary |
| `4` | Tables & Indexes | Schema size totals, a table list (dead-tuple %, xid age, seq-scans/hour, last autovacuum), unused/invalid indexes (with reclaimable size), and missing-index candidates (unindexed foreign keys, high seq-scan-ratio tables) |
| `5` | Triggers | Every user-defined trigger (`pg_trigger`, excluding internal foreign-key-backing ones) — schema, table, function, enabled/disabled state — with an always-visible detail pane showing that trigger's DDL (`pg_get_triggerdef`) and its function's full source (`pg_get_functiondef`) |
| `6` | Playground | A psql-style REPL against the live connection, on its own dedicated database connection so a slow query here never freezes the other tabs' live monitoring. The transcript and live input share one bordered box, no seam between output and input. A scrolling transcript (`dbname=>` prompts, a plain `->` for continuation lines) shows every command run this session; `enter` runs a `;`-terminated statement, otherwise starts a continuation line. Non-`SELECT` statements need a second `enter` to confirm before they run; `tab` completes SQL keywords/schema/table names (cycling on repeat); `ctrl-c` cancels a running query (or clears the buffer if nothing's running); `↑`/`↓` recall history inline. Result tables match real `psql`'s own aligned format. A single bare `SELECT` loads 200 rows at a time — scroll (`PageDown`) to the bottom to fetch the next page, same idea as DBeaver's chunked fetch. A handful of readline/nvim-insert-mode chords (`ctrl-w`/`u`/`k`, word/buffer jumps) round out the editor — see §7. The transcript and drafts are in-memory only — nothing is written to disk |

---

## 4. Connecting

### 4.1 Saved Connections (Picker / Wizard)

**Run `pgpilot` with no flags** and it acts like `rclone`'s remote picker: if you have
saved connections, it lists them for you to pick from (or add a new one); if you have
none yet, it walks you straight into a short wizard (name, host, port, user, database)
and saves the result for next time.

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

Saved profiles store host/port/user/dbname/SSL settings only — **never the password**.
Password comes from `PGPASSWORD` if set, otherwise you'll get a masked `Password:`
prompt (empty input means "no password needed," e.g. trust/peer auth). Profiles live in
`~/Library/Application Support/pgpilot/config.toml` on macOS (via the `directories`
crate; XDG-style paths on Linux), permissioned `0600`.

- `--profile <name>` connects directly using a saved profile by name, skipping the
  picker (still prompts for password if `PGPASSWORD` isn't set).
- `e` at the picker, then a connection number, re-opens the wizard for that connection,
  pre-filled with its current values — blank input keeps a field as-is, `-` clears an
  optional field (CA/client cert/key paths). Saves and connects when done.

### 4.2 SSL / Mutual TLS

When adding or editing a connection, answering `y` to "Use SSL?" prompts for three
optional PEM paths, each independently skippable:

- **CA root cert** — verify the server's certificate against this CA. If left unset, the
  connection is still encrypted but the server's certificate isn't verified (equivalent
  to `sslmode=require`, not `verify-full`).
- **Client cert** + **client key** — for mutual TLS, if your server requires a client
  certificate. Both must be set together, or neither.

The same options are available non-interactively via `--ssl`, `--ssl-root-cert`,
`--ssl-client-cert`, and `--ssl-client-key` (only apply to `--host`/`--user`/etc.-style
flags, not `--dsn` — see below).

### 4.3 Flag-Driven (Scripting / CI)

For scripting/CI, or if you'd rather not use saved profiles at all, the original
flag-driven usage works exactly as before and never prompts for anything:

1. `--dsn <connection-string>` — full override, e.g.
   `--dsn "postgres://user:pass@host:5432/dbname?sslmode=prefer"`. Takes precedence over
   everything below. Note: `--ssl*` flags don't apply to `--dsn` — encode SSL params in
   the DSN string itself if you need them here (`sslmode=`, `sslrootcert=`, etc. are
   libpq-native).
2. Otherwise built from individual flags, each with an env var fallback:
   - `--host` / `PGHOST` (default `localhost`)
   - `--port` / `PGPORT` (default `5432`)
   - `--user` / `PGUSER` (default: `$USER`)
   - `--dbname` / `PGDATABASE` (default: same as user)
   - `--ssl`, `--ssl-root-cert <path>`, `--ssl-client-cert <path>`,
     `--ssl-client-key <path>` — same semantics as the wizard's SSL prompts above.
3. `PGPASSWORD` is read directly from the environment (not a `--password` flag, so it
   never shows up in shell history or `--help` output).

Giving any of `--dsn`/`--host`/`--port`/`--user`/`--dbname` (as a flag or via its env
var) bypasses the picker/wizard entirely — only a fully bare `pgpilot` (or `--profile`)
goes interactive.

### 4.4 Runtime Flags

`--interval <secs>` sets the fast-tier poll interval (default `2`), independent of how
the connection was resolved. Adjustable at runtime with `-`/`+` — see [Keys](#7-keys).

`--ascii` swaps every Unicode block/braille glyph (sparklines, bars, cursors, the
blocking-tree's connectors) for plain ASCII, for terminals/fonts that don't render them
cleanly.

### 4.5 Examples

```bash
pgpilot                    # picker/wizard
pgpilot --profile prod     # saved profile, no picker
pgpilot --dbname postgres  # flag-driven, no prompts
PGPASSWORD=secret pgpilot --dsn "postgres://user@host/db"
pgpilot --ascii            # plain-ASCII glyphs
```

---

## 5. Flags & Options

```
Live TUI dashboard for Postgres usage/utilization

Usage: pgpilot [OPTIONS]

Options:
      --dsn <DSN>
          Full connection string, overrides all other connection flags
      --host <HOST>
          [env: PGHOST=]
      --port <PORT>
          [env: PGPORT=]
      --user <USER>
          [env: PGUSER=]
      --dbname <DBNAME>
          [env: PGDATABASE=]
      --profile <PROFILE>
          Named saved connection to use directly, skipping the picker
      --ssl
          Use SSL for this connection (only applies to --host/--user/etc, not --dsn)
      --ssl-root-cert <SSL_ROOT_CERT>
          CA cert (PEM) to verify the server against; if omitted, SSL still encrypts but doesn't verify the server certificate
      --ssl-client-cert <SSL_CLIENT_CERT>
          Client certificate (PEM), for mutual TLS — requires --ssl-client-key too
      --ssl-client-key <SSL_CLIENT_KEY>
          Client private key (PEM), for mutual TLS — requires --ssl-client-cert too
      --interval <INTERVAL>
          Refresh interval in seconds [default: 2]
      --ascii
          Use plain ASCII glyphs instead of Unicode block/braille characters, for terminals/fonts that don't render them cleanly
  -h, --help
          Print help
```

---

## 6. Core Concept

PgPilot treats a Postgres instance as three questions, each polled on its own clock:

1. **What's happening right now?** — connections, cache I/O, activity, indexes,
   per-database stats. Polled at the `--interval` rate (default 2s, adjustable live with
   `-`/`+`).
2. **What's slow?** — `pg_stat_statements`, polled every 15s; skipped cleanly (zero
   queries) if the extension isn't installed.
3. **What's degrading structurally?** — dead tuples, xid age, unindexed foreign keys,
   triggers. Polled every 5 minutes — catalog-heavy data that doesn't need sub-second
   freshness, and polling it that often would make the monitor itself a load problem.

Every query in every tier is fetched and reported independently — if one breaks (a
permission issue, a Postgres-version-specific column), only that block shows an error;
every other block on every other tab keeps updating.

The diagnosis engine (`g`) doesn't add new queries on top of any of this. It's a pure,
synchronous pass over data the three tiers above are already fetching for their own
panels — ranked against known incident shapes (xid wraparound, checkpoint storms,
connection pressure, lock chains, disk spill, replication lag, N+1 query signatures)
instead of leaving you to notice the raw numbers yourself.

---

## 7. Keys

| Key | Action |
|---|---|
| `1`–`6` | Switch tab |
| `↑`/`↓` or `j`/`k` | Scroll/select rows on the current tab (Queries, Activity, Tables & Indexes, Triggers) |
| `PageUp` / `PageDown` | Scroll the detail pane's text (Queries, Activity, Triggers, and Playground's transcript) when it overflows the pane |
| Mouse wheel | Scroll the detail pane under the cursor (Queries, Activity, Triggers — also works once it's zoomed into a full-screen popup) or the Playground transcript (whenever that tab is active, regardless of cursor position) |
| Click a row | Select it (Queries, Activity, Tables & Indexes, Triggers) — same effect as moving `j`/`k` onto it |
| Click a tab | Switch to it — same effect as its `1`–`6` key |
| Click detail pane | Zoom the detail pane (Queries, Activity, Triggers) into a full-screen popup — `esc`/`q` closes it, `PageUp`/`PageDown`/mouse wheel still scroll while it's open |
| `s` | Cycle sort (Queries: total time → mean time → calls → disk I/O; Tables & Indexes: size/name, press again to reverse) |
| `x` / `X` | Cancel / terminate the selected Activity row's backend (`pg_cancel_backend`/`pg_terminate_backend`) — real, immediate, no confirmation prompt, same spirit as `htop`'s kill. Requires the `pg_signal_backend` role (or superuser); otherwise the attempt fails with a status message, not a crash |
| `space` | Pause/resume polling |
| `-` / `+` | Slow down / speed up the fast-tier poll rate |
| `r` | Force an immediate refresh, without waiting for the next poll tick (works even while paused) |
| `d` | Open a full-screen database picker (owner, size, sessions, tps, cache hit, state); `enter` reconnects to the selected one (same host/user/SSL, just a different `dbname`), `esc`/`q`/`d` closes it without switching |
| `g` | Open the diagnosis popup (headline, ranked suspects, suggested fixes). `g`/`esc`/`q` closes it |
| `e` | View the full text of the current error(s) — only active when the footer shows a red error, since the footer's single line truncates long Postgres error messages. `e`/`esc`/`q` closes it |
| `q` | Quit |

`d` only shows up (and works) when connected via a saved profile or
`--host`/`--user`/etc. flags — a raw `--dsn` connection string can't be safely rewritten
to point at a different database, so switching is unavailable in that mode.

**The Playground tab is a psql-style REPL**: a scrolling transcript of every command you've run this
session (`dbname=>` for the first line of a statement, a plain `->` for a continuation line — no
repeated database name, unlike real `psql`'s own prompt), with the live input pinned below it — both
render inside one shared bordered box, so there's no seam between output and input, matching a real
terminal. The editor captures every plain key (so typing SQL never triggers the
shortcuts above) — only these do something else:

| Key | Action |
|---|---|
| `enter` | Runs the current statement once it ends in `;` (psql's own rule) — otherwise inserts a newline for a continuation line (shown with the `->` prompt). On a non-`SELECT` statement, the first press shows a confirm warning instead of running; press it again to actually execute |
| `F5` / `ctrl-enter` / `cmd-enter` | Force-run whatever's in the buffer right now, regardless of a trailing `;` — an escape hatch for the rare case the `;`-completeness check doesn't fire when you expect (e.g. it doesn't understand a `;` inside a string literal). `ctrl-enter`/`cmd-enter` need a terminal that supports the Kitty keyboard protocol (kitty, WezTerm, Ghostty, newer iTerm2, etc.) — `F5` always works, everywhere |
| `tab` | Completes the SQL keyword/schema/table name before the cursor (e.g. `SEL` → `SELECT`, `use` → `users`, `aud` → `"Audit"` for a schema that needs quoting) — press again to cycle through other matches |
| `ctrl-c` | psql's own dual-purpose binding: cancels a running query (`pg_cancel_backend` under the hood), or — if nothing's running — discards whatever's currently typed (including a multi-line continuation) and gives you a fresh prompt |
| `↑` / `↓` | At the top/bottom row of the current input, recalls the previous/next command from this session's history into the editor (readline-style); mid-buffer they still just move the cursor, so editing a multi-line query works normally |
| `ctrl-q` | Quit (plain `q` is left free to type, e.g. `...FROM queue`) |
| `esc` | Leave the tab (back to Overview) — or, if a confirmation is pending, back out of that first |
| `PageUp` / `PageDown` | Scroll the transcript |

A few readline/nvim-insert-mode-style chords are also available for editing, since the editor itself
stays non-modal (no normal/insert mode to switch between — see `src/editor.rs`):

| Key | Action |
|---|---|
| `ctrl-w` | Delete the word before the cursor |
| `ctrl-u` | Delete from the cursor to the start of the line |
| `ctrl-k` | Delete from the cursor to the end of the line |
| `ctrl-←` / `ctrl-→` | Jump a word left/right (nvim's `b`/`w`) |
| `ctrl-Home` / `ctrl-End` | Jump to the start/end of the buffer (nvim's `gg`/`G`) |

pgpilot enables mouse reporting for the click/scroll interactions above, which is also what most
terminals use to gate native click-drag text selection — copying text out via your terminal's own
selection may need a modifier-held drag (e.g. Option-drag in Terminal.app/iTerm2) while pgpilot is
running, depending on your terminal.

---

## 8. Output Behavior

- **Partial failure, not total failure** — if a query fails (a permission issue, a
  Postgres-version-specific column), only that block shows it: a red `✗` in its title
  and the error inline, while every other block on every other tab keeps updating
  normally.
- **Reconnect, not blackout** — if the connection itself drops, every block shows the
  error and the last-known-good data stays on screen (stale but visible) until the
  background poller reconnects automatically on the next tick.
- **Database switching** works the same way under the hood — a fresh connection opens
  before the old one drops, and every tab shows its loading spinner again until the
  first snapshot from the new database arrives.
- **Version skew is handled, not ignored** — Overview's checkpoint/buffer stats
  automatically use the right system view for the connected server's version (Postgres
  17 moved those columns from `pg_stat_bgwriter` to a new `pg_stat_checkpointer` view).
- A small Braille spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) shows up in two places: on the plain terminal
  while the initial connection is being made, and inside any panel still waiting on its
  first query result.

---

## 9. Platform Support

| Platform | Install | Notes |
|---|---|---|
| macOS (Intel + Apple Silicon) | `install.sh` or `cargo build --release` | |
| Linux (x86_64) | `install.sh` or `cargo build --release` | |
| Windows (x86_64) | `choco install pgpilot`, `.zip` from [Releases](https://github.com/kavin-vs/pgpilot/releases), or `cargo build --release` | Chocolatey package pending its first moderation review; the `.nupkg`/`.zip` release assets work meanwhile |

Tested against Postgres **13 through 17**.

---

## 10. Development

Run straight from source with `cargo run` — no need to build a release binary while
iterating. Anything after `--` is passed through to the app:

```bash
cargo run                          # bare invocation: saved-connection picker / wizard
cargo run -- --dbname postgres     # flag-driven, connects straight in, no prompts
cargo run -- --profile local       # connect using a profile you've already saved
```

This needs a real Postgres to talk to. If you don't have one running, the fastest local
option on macOS is Homebrew:

```bash
brew install postgresql@14
brew services start postgresql@14
createdb $(whoami)                 # or any dbname you'll pass with --dbname
```

Other useful commands while developing:

```bash
cargo check           # fast type-check, no codegen
cargo clippy           # lint
cargo build --release # optimized binary at target/release/pgpilot
```

Quitting the TUI (`q`) always restores your terminal, even on a crash (`ratatui::init()`
installs a panic hook for this) — so it's safe to Ctrl-C out of `cargo run` too if
something hangs.

If you use [Claude Code](https://claude.com/claude-code), this repo ships three skills
under `.claude/skills/` that encode this project's own workflows — `add-panel` (the
checklist for wiring a new dashboard data source/tab), `debug-pgpilot` (known failure
patterns: PG version-skew queries, terminal corruption from stray output, TLS build
deps), and `release-pgpilot` (cutting a version bump). They trigger automatically on
matching requests; no setup needed.

---

## 11. Scope

In: the six tabs above (including Playground, a psql-style REPL on its own dedicated
connection), the diagnosis popup, saved connection profiles (list/add/edit/pick,
SSL/mutual-TLS), the database picker, cancel/terminate.

Out of scope (candidates for future versions): deleting a saved profile in place
(listing, adding, and editing are all supported), true time-windowed wait-event
profiling (current sampling is a bounded recent-window point-sample, not
`pg_wait_sampling`-grade), query-plan-derived index suggestions (missing-index
candidates are limited to two mechanically-derivable heuristics — unindexed foreign keys
and high seq-scan-ratio tables — not fabricated column-level `CREATE INDEX` guesses), and
— for Playground — persisting the transcript/drafts to disk, full vim modal editing (normal/insert/
visual modes — the editor stays non-modal, with a handful of readline/nvim-insert-mode chords
instead, see §7), real SQL tokenization for the confirm-guard's statement split and the
`enter`-runs-on-`;` completeness check (both a naive top-level `;` split today, which doesn't
understand semicolons inside string/dollar-quoted literals), pagination for anything beyond a
single bare `SELECT` (multi-statement scripts, `WITH`, DML/DDL still fetch everything in one
shot — the current pagination is stateless LIMIT/OFFSET re-fetching, not a held server-side
cursor, so very deep scrolling on an expensive query re-runs it from scratch each page), the
literal Postgres command tag on a non-`SELECT` result (shows a row count, not `psql`'s
`CREATE TABLE`/`UPDATE 3` text — the underlying driver API doesn't expose it), and matching
`psql`'s blank-for-`NULL` default (kept as literal `NULL` text on purpose — `psql`'s own
default makes `NULL` and an empty string look identical, a well-known footgun). Tab-completion
covers SQL keywords, schema names, and table names only — no column-name completion (no existing
data source for full column lists), and no DBeaver-style dropdown popup (inline cycling instead —
see §7).
