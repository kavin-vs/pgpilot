---
name: debug-pgpilot
description: "Use when something is broken in pgpilot itself (the Postgres monitoring TUI in this repo) — a build failure, a stuck/corrupted UI, or a query error. Triggers: 'pgpilot won't build', 'a panel is stuck loading in pgpilot', 'TUI screen looks corrupted', 'query fails only on some Postgres version', 'pgpilot shows a generic db error'."
---

# Debug pgpilot

Known failure-pattern classes for this codebase, each already hit and fixed once — check these before writing a new fix, so the same root cause doesn't get refixed in two places.

- **One panel never loads while its siblings work fine** → check `app.errors` for that panel's `source_label`. Since v2, every query is independently `send_labeled`'d specifically so one broken query can't blank unrelated panels. If it's still happening, look for a query that got accidentally re-bundled into another's `?`-chain.

- **"column X does not exist" on some Postgres clusters only, not others** → version skew, not a logic bug. `pg_stat_bgwriter` dropped `checkpoints_timed`/`checkpoints_req`/`buffers_checkpoint` in PG17 (moved to `pg_stat_checkpointer`); I/O timing columns were renamed in PG17 too. Check for missing `pg17_plus` branching (precedent: `db/cache_io.rs::fetch_bgwriter`, `db/statements.rs::query`) before assuming anything else. Test against both an old and new cluster (throwaway `initdb`, not the real local server).

- **TUI screen looks corrupted, or a stuck-looking error banner appears during disconnect** → something wrote directly to stdout/stderr after `ratatui::init()` — the alt screen owns the terminal at that point and direct writes look like application bugs even when app state is fine. Grep `src/` for stray `print!`/`println!`/`eprintln!` outside the two documented pre-TUI call sites (the connect spinner in `main.rs`). Route anything needing surfacing through `AppEvent` instead.

- **Build fails compiling `aws-lc-sys`** → missing `cmake` or a C compiler on `PATH` (needed because TLS support pulls in `aws-lc-sys`, rustls's default crypto provider). Documented fallback if the build environment truly can't have those: switch rustls's feature flags to `ring` instead of `aws-lc-sys` — not yet applied anywhere in this repo.

- **Conninfo string breaks on a password containing a space or quote** → any hand-built conninfo string must go through `quote_conninfo_value()` (`src/cli.rs`) — never `format!` a raw value directly into one. libpq keyword/value conninfo strings are whitespace/quote-sensitive.

- **Error message just says "db error" with no useful detail** → `anyhow::Error`'s bare `{}`/`.to_string()` only shows the outermost wrapper, not the actual Postgres ERROR/DETAIL/HINT text one level down in `source()`. Use `format!("{e:#}")` for `anyhow::Error`s (walks the chain), or the hand-rolled `chained_message()` for a raw `tokio_postgres::Error` (e.g. `run_signal`'s `client.execute()` return, which isn't anyhow-wrapped).

- **Borrow-checker fight: need to read an enum then mutate `App`** → match on `&app.field` in a small scope before taking `&mut App`, rather than trying to do both at once (precedent: `ui/queries.rs` checks `matches!(&app.statements, ...)` before taking `&mut App` for the table).

Before writing a fix: check whether this exact root-cause class is already named in CLAUDE.md's `db/mod.rs`/`db/cache_io.rs` sections — version-skew and stray-print bugs have each already happened once and been fixed at their root (a shared function), not per-caller.
