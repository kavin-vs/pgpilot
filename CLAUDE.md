# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`PgPilot` (crate/binary: `pgpilot`) is a lightweight, interactive Rust CLI that gives a live, at-a-glance view of Postgres usage/utilization — connections, cache hit ratio, index health, and schema/table sizes — in the spirit of `htop`/`k9s`, but for Postgres. It connects directly via a native Rust driver (`tokio-postgres`), not by shelling out to the `psql` binary.

Note: the on-disk repo directory is still named `psql-cli` (that's the project's original name before the rename to PgPilot) — only the crate/binary/docs were renamed, not the directory.

Design plans (v1 dashboard: crate choices, SQL, async architecture; rename + saved-profiles feature): `/Users/kavinvs/.claude-personal/plans/im-building-an-lightweight-dapper-candle.md` — the file was reused/overwritten for the second plan, so it only reflects the most recent one; this CLAUDE.md is the durable record of what actually landed.

## Commands

- Build: `cargo build`
- Run: `cargo run -- [flags]` (e.g. `cargo run -- --dbname postgres`)
- Check without building: `cargo check`
- Lint: `cargo clippy`
- Test: `cargo test`

Build requirement (resolved): TLS used to default to `aws-lc-sys` (rustls's default crypto provider), which compiles a C library via `cmake` and needs `cmake` + a C compiler on `PATH`. That was fine on the one dev machine this ran on, but broke down once release CI needed to cross-build for `windows-latest` — GitHub's Windows runner image doesn't ship NASM, which `aws-lc-sys` wants for its assembly path. `Cargo.toml` now pins `rustls`/`tokio-postgres-rustls` to the `ring` crypto backend instead (`default-features = false, features = ["ring", "std", "tls12", "logging"]` / `features = ["ring"]`) — `ring` only needs a plain C compiler, which every platform's Rust toolchain already has, so all 4 release targets (see Release below) build with zero extra CI setup. No code changes were needed for the swap: `src/db/tls.rs` never calls `CryptoProvider::install_default()` explicitly, it relies on rustls's "exactly one crypto feature compiled in" implicit default, which works the same regardless of which backend that one feature is. Verified end-to-end against a throwaway local Postgres with `ssl = on` and a real (non-CA) self-signed cert, both the accept-any-cert (`--ssl`) and CA-verified (`--ssl --ssl-root-cert`) paths.

### Versioning & releases

Cargo.toml's `version` is the single source of truth for semver. Follows semver for CLI flags and the saved-profile `config.toml` schema; TUI layout/rendering is explicitly *not* part of that contract — the UI has changed every internal version bump so far (see the numbered notes below) and isn't a stability guarantee. Staying on `0.y.z` is deliberate (semver's own major-zero rule: "initial development, anything may change") — appropriate for a CLI with no test suite and no declared-stable schema yet; `1.0.0` is a future decision, not implied by any release so far.

Release process: `.claude/skills/release-pgpilot/SKILL.md` covers the pre-tag checklist (clean tree, `cargo build`/`cargo clippy` clean, version bump, smoke test). Pushing a `vX.Y.Z` tag (after asking the user — pushing is a shared/visible action) triggers `.github/workflows/release.yml`, which cross-builds 4 targets (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc` — the two macOS targets both build from one `macos-latest` runner via cross-compilation, since Apple's toolchain links both arches natively) and publishes a GitHub Release with all 4 binaries attached via `gh release create --generate-notes` (no CHANGELOG.md exists — GitHub's auto-generated notes from commit/PR history cover that gap). A `check-version` job fails the whole run if the pushed tag doesn't match `Cargo.toml`'s version, so tag/version drift can't silently ship. CI's `build` job and `scripts/release.sh` (a manual fallback for when CI isn't available — see the skill) both call the same `scripts/build-target.sh <target>` for the actual build+package step, so there's exactly one implementation of "how to build a release binary," not two that can quietly drift apart; the manual path only builds what the current OS can natively produce (no Windows support running it locally) and still requires the tag to already exist — it doesn't push one for you. `install.sh` (repo root) is a `curl | sh` installer mirroring rustup/Deno's — downloads the right release asset for the caller's OS/arch and drops it in `~/.local/bin`; Windows users get a Chocolatey package (see below) plus the `.zip` release asset as a manual fallback. Homebrew distribution was deliberately not set up — would need a second public tap repo, deferred unless there's real demand.

Windows also gets a Chocolatey package (`choco/pgpilot.nuspec` + `choco/tools/chocolateyinstall.ps1`), added on direct user request ("make windows version also ease [sic]") after checking a sibling Rust CLI repo (`~/Personal/repo/rust/cache_cleaner`) for prior art to reuse — it had none (plain `cargo` project, no CI/packaging), so this was built from scratch rather than ported. Deliberately **not** an embedded-binary package: `chocolateyinstall.ps1` downloads the same `pgpilot-vX.Y.Z-x86_64-pc-windows-msvc.zip` the release workflow already builds, via `Install-ChocolateyZipPackage` (which also auto-generates the PATH shim for `pgpilot.exe` — no separate uninstall script needed, Chocolatey tracks and removes what it installed). Checksum verification is **dynamic, not hand-maintained per release**: the `publish` job now runs `sha256sum` over every `dist/` asset into a `checksums.txt` uploaded alongside the binaries, and the install script fetches that file at *install time*, greps the line matching its own zip name, and feeds the hash to `-Checksum64` — chosen specifically so a new pgpilot version needs zero edits to the `.ps1` (only the nuspec `<version>`, set via `choco pack --version` at pack time, has to change). A new `choco-pack` job (`windows-latest`, since the `choco` CLI needs Windows/Mono — `scripts/release.sh`'s manual fallback stays Unix-only and does not attempt this) runs after `publish`, packs the `.nupkg`, and always attaches it to the GitHub release (`choco install pgpilot -s <folder>` works against that immediately, no account needed) — pushing to the actual Chocolatey community repo (`choco push … --api-key`) only runs when `CHOCO_API_KEY` is non-empty (checked *inside* the pwsh script, not in the step's `if:` — see the incident note below), so nothing publishes to the moderated public feed until that secret is deliberately added; first-time publish there also goes through Chocolatey's manual moderation review (hours-to-days), same category of external commitment as the deferred Homebrew tap above — scaffolding shipped now, actual `choco.org` submission is a separate step for whenever the user creates that account.

**Incident (v0.1.2, caught after the repo went public and `install.sh` started 403'ing / finding no release)**: this `choco-pack` step originally gated on `if: ${{ secrets.CHOCO_API_KEY != '' }}` directly on the step. GitHub Actions rejects the `secrets` context inside any step/job `if:` at schema-validation time — and when one workflow file fails that validation, GitHub doesn't just skip the offending job, it fails to dispatch the *entire file* for *every* push from then on (silently: no run even appears for a tag push, and an ordinary branch push gets a zero-job "workflow file issue" failure with no useful diff shown). This had been broken since the commit that added Chocolatey packaging (Aug 23), which is also when `v0.1.1` was tagged — so that tag's push never triggered a build, no GitHub Release was ever created for it, and `install.sh` had nothing to fetch (unrelated to the repo's public/private state — same failure would've happened public from day one). Fixed by moving the secret into the step's `env:` unconditionally and branching on that env var inside the pwsh script instead (`if ([string]::IsNullOrEmpty($env:CHOCO_API_KEY)) { ... exit 0 }`) — `secrets` are fine to reference in `env:`, just not in `if:`. `actionlint` (not previously run against this repo) catches this class of error statically; worth running against any future workflow edit here before pushing a tag against it, since a broken workflow file fails closed and silent rather than loud.

Fixing that surfaced a second, independent bug in the same run (`v0.1.2`): all 4 `build` jobs succeeded, but `publish` failed with `fatal: not a git repository` — the `publish` job ran `gh release create ... --generate-notes` with no preceding `actions/checkout` step, and `gh` needs a real `.git` checkout to resolve the release target/generate notes even though `GH_TOKEN`+`GITHUB_REPOSITORY` alone are enough for most `gh` calls in Actions. Fixed by adding `actions/checkout@v4` as `publish`'s first step. Since a tag's workflow file is frozen at that tag's commit (pushing the *same* tag again re-runs the *old*, still-broken file — confirmed by this bug only showing up after the `secrets`-in-`if:` fix let the file dispatch at all), each of these needed a fresh patch tag rather than a re-push of `v0.1.2`: `v0.1.1` and `v0.1.2` both exist as tags with no corresponding GitHub Release and are left as-is (harmless orphans, not deleted per the release skill's "fix forward" rule) — `v0.1.3` is the first tag with a fully working `check-version → build → publish` pipeline.

### Connecting

Resolution order lives in `main.rs::resolve_conninfo()`, returning `(conninfo: String, tls: TlsMode, conn_parts: Option<ConnParts>)`. `ConnParts` (`src/cli.rs`) is `{host, port, user, password, dbname}` — `dbname` kept separate from the rest specifically so the in-app database-switch popup (see below) can rebuild a conninfo against a different database without touching host/user/password. It's `None` only for a raw `--dsn` connection, which is an opaque string that can't be safely taken apart.
1. `--dsn`, or any of `--host`/`--port`/`--user`/`--dbname` (flag or `PG*` env var) → `Cli::connection_string()` (`src/cli.rs`), same defaulting logic as before this feature existed (localhost/5432/$USER/etc.), with TLS from the `--ssl*` flags via `Cli::tls_mode()`; `Cli::conn_parts()` builds the same defaulted parts (or `None` for `--dsn`) and both methods share the defaulting logic to avoid duplicating it. No prompts — this path is unchanged from pre-profile behavior, so scripts/CI stay unaffected. **Exception**: `--dsn` itself always gets `TlsMode::Disabled` regardless of `--ssl*` — we don't parse sslmode out of an arbitrary DSN string; a `--dsn` user who needs TLS encodes it in the DSN itself (documented limitation, README).
2. `--profile <name>` → look up that name in the saved config (`src/config.rs`); errors out (before entering the alt screen) if not found.
3. Bare invocation → `onboarding::list_and_pick()` (`src/onboarding.rs`): shows saved profiles to pick from (`[SSL]` tag if enabled), edit one (`e`, then a connection number), add new, or quit; if none are saved yet, goes straight into `run_wizard()`.

For cases 2–3, TLS comes from the resolved `Profile`'s ssl fields, and password resolution happens after host/port/user/dbname are known: `PGPASSWORD` env if set, else `onboarding::prompt_password()` (masked, via `rpassword`). Case 1 keeps the original env-only password behavior — no surprise prompt in non-interactive use.

`--interval <secs>` sets the dashboard poll interval (default `2`), independent of connection resolution.

## Architecture

**v18**: Triggers' "Selected Trigger" pane, an always-visible inline block since v7, moved to a
full-screen scrollable popup instead, on direct user request ("make the trigger query in full
screen modal with scroll inside instead of having in bottom"). `ui/triggers.rs::draw()` no longer
splits the tab into a `[Min(8), Percentage(35)]` table/detail pair — the table now takes the whole
tab (`app.detail_pane_rect = None` always, since there's no inline pane left to click-zoom or hit-
test) and `draw_detail`'s content (trigger DDL + full function body) only renders inside
`draw_detail_popup`, reusing the exact full-screen-`Clear`-then-`draw_detail` mechanism v10 already
built for Queries/Activity's click-to-zoom — no new rendering code. Since there's no pane to click
anymore, opening it needed a new trigger: plain **`enter`**, gated to `app.active ==
PanelKind::Triggers` and a non-empty `app.triggers` (`main.rs::handle_key`), sets the same
`App::detail_popup_open` flag Queries/Activity's mouse click sets — closing (`esc`/`q`) and
scrolling (`PageUp`/`PageDown`, mouse wheel) inside it are all pre-existing `detail_popup_open`
handling, untouched. `main.rs::handle_mouse_click`'s `has_detail_pane` check dropped `Triggers`
(the click-zoom path is now Queries/Activity-only — `detail_pane_rect` being permanently `None` for
Triggers already made a click there a no-op, this just makes the dead branch explicit) but its
`has_table`/row-select path keeps `Triggers` as before, since the table itself is unaffected.
Footer help text (`ui/widgets.rs::draw_footer`) dropped Triggers from the `PgUp/PgDn: scroll detail`
hint (only true for Queries/Activity's inline panes now) and gained its own `enter: view trigger`
hint when that tab is active.

**v17**: background auto-update, plus an unrelated `install.sh` reliability fix, both from a
single direct user report ("implement auto update option ... also if i run mac installer again it
is not showing any message -- stuck in there"). **`install.sh`**: both `curl` calls (the
`releases/latest` redirect resolve, and the tarball download) had no `--connect-timeout`/
`--max-time`, and `-s` suppresses all curl output — a stalled connection (e.g. GitHub's edge
throttling a second rapid request from the same IP shortly after a first run, the same *class* of
problem `0b916fe` already fixed once for `api.github.com`'s hard rate limit) hung the script with
zero visible output. Fixed with `--connect-timeout 10 --max-time 30|60 --retry 2 --retry-delay 1`
on both calls, plus one new `echo` before the redirect resolve (previously the only silent-until-
success step, since the first pre-existing `echo` is after it). No new state/lock file — the root
cause was "no ceiling on a network call," not "no memory of a prior run."

**Auto-update** (`src/update.rs`, new module, pure logic no UI references — same shape as
`src/db/*.rs`'s standalone `async fn(...) -> Result<T>` modules): on by default, `--no-update-check`
(`src/cli.rs`) opts out. `update::update_check_task(tx)` is spawned fire-and-forget from `main()`
after `poll_task`'s own spawn (so it can never delay getting into the Postgres session — same
fire-and-forget precedent `poll_task`/`playground_task` already set), throttled to at most once per
24h via a `last_checked_unix` timestamp in a new sibling state file, `update_state.toml` (same
`ProjectDirs::from("", "", "pgpilot").config_dir()` + `0o600`-on-unix pattern `src/config.rs`
already established — deliberately a separate file, not added to `config.toml`/`Config`, which
stays scoped to connection profiles only, same reasoning already documented for why Playground's
state doesn't live there either). The throttle exists for the exact same reason as the `install.sh`
fix above: `self_update`'s GitHub backend calls `api.github.com` directly (confirmed live — a probe
during development hit that same unauthenticated rate limit), so an unthrottled per-launch check
would risk the identical failure class. **Load-bearing fix caught during manual verification**: the
timestamp must be persisted even when the check *fails* (rate-limited, offline, etc.) — an earlier
version only saved it on success, which meant a persistent failure would retry on every single
launch instead of backing off, i.e. exactly the rate-limit storm the throttle exists to prevent;
`check_and_stage_blocking` now always calls `save_state` after `fetch_and_stage`, whatever its
`Result`.

On a `due_for_check` launch: `self_update::backends::github::Update::configure()...is_update_available()`
checks for a newer release than `env!("CARGO_PKG_VERSION")`; if found, a second `Update` (same
builder, plus `.bin_install_path(...)` and `.checksum_from_asset("checksums.txt")`) downloads and
checksum-verifies the matching platform asset — using this repo's own release assets exactly as
`release.yml` publishes them (`pgpilot-{tag}-{target}.tar.gz`/`.zip` + `checksums.txt`, all
resolved automatically by `self_update`'s own target-string asset matching, no target override
needed). Crucially, `bin_install_path` points at a **side path**
(`<config_dir>/update_staged/pgpilot[.exe]`), never `current_exe()` — the background task never
touches the binary a live TUI session is running from. `self_update` is fully synchronous
(its async support is a separate, unused `"async"` feature), so the whole check+download runs
inside `tokio::task::spawn_blocking` — no `tokio` `"net"` feature needed. On success, reuses the
**existing** footer status mechanism as-is: `AppEvent::Status(format!("Update v{v} ready — restart
to apply"), StatusLevel::Info)` over the same `tx` channel every other background task already
sends on — no new `AppEvent` variant, no new `App` field, no `apply_event`/UI changes needed. On
any failure, silent (no status message) — a background update hiccup shouldn't compete with real
DB feedback on the one status line.

The actual swap happens only at the **next process's** startup: `update::apply_staged_update_if_present()`
is the literal first thing `main()` does (before `Cli::parse()`, before `ratatui::init()`) — if a
staged version exists and is still genuinely newer (`self_update::version::bump_is_greater`,
defensive against an already-applied or downgrade-edge-case marker), `self_replace::self_replace(&staged_path)`
swaps the running executable's file in place (the `self-replace` crate exists specifically to make
this safe cross-platform, including the Windows can't-delete-a-running-exe case) and clears the
marker; otherwise the stale marker/file is just cleaned up. Errors here are logged to stderr (still
safe pre-`ratatui::init()`) and swallowed — a corrupted staged update must never block startup.

**Dependency choice, load-bearing**: `self_update` is configured `default-features = false` +
`features = ["ureq", "rustls", "github", "checksums", "archive-tar", "compression-tar-gz",
"archive-zip", "compression-zip-deflate"]` — deliberately **not** its default `reqwest` backend.
Confirmed by reading both crates' actual `Cargo.toml`s: `reqwest`'s own `rustls` feature now
hard-selects the `aws-lc-rs` crypto provider (no `ring` option exposed anymore), which would pull
in `aws-lc-sys` and reintroduce the exact NASM/cmake cross-build problem `rustls`/
`tokio-postgres-rustls` were already pinned to `ring` to avoid (see the Build requirement note
above) — silently breaking the Windows/macOS release CI targets. `ureq`'s `rustls` feature uses
`ring` instead, matching this repo's existing TLS backend choice exactly. Verified end-to-end after
implementation: `cargo tree -i aws-lc-sys` and `cargo tree -i openssl-sys` both report neither crate
in the dependency tree, and `cargo build`/`cargo clippy --all-targets -- -D warnings` are clean.

**v16**: mouse-wheel scroll and click extended to the rest of the app — wheel scrolling on all 4
scrollable panes (previously `PageUp`/`PageDown`-only), clicking a table row to select it (previously
keyboard-only, an explicit v10 ceiling: "clicking a table row does nothing"), and clicking a tab to
switch (previously `1`-`6`-only). Direct user request, following straight from v10's original mouse
support. No new terminal negotiation needed — `EnableMouseCapture` (`main.rs`, since v10) already
reports `MouseEventKind::ScrollUp`/`ScrollDown` once wired, confirmed against crossterm 0.29's
vendored source.

**Scroll**: `handle_mouse` (`main.rs`) is restructured into `handle_mouse_scroll`/`handle_mouse_click`,
dispatched on `mouse.kind`. The overlay guard changed in one specific way: `detail_popup_open` (the
zoomed-detail-pane state) used to block *all* mouse input; it now blocks only the click branch (click
must stay a no-op while already zoomed, unchanged) — scroll passes through, since the zoomed popup
*is* the pane and the wheel should keep scrolling it exactly like `PageUp`/`PageDown` already do while
zoomed. For Queries/Activity/Triggers, scroll is hit-tested against the existing
`App::detail_pane_rect` (reused as-is, no new `Rect`) unless zoomed, in which case there's nothing to
hit-test (full screen). Playground's transcript has no stored `Rect` (unlike the 3 detail panes) —
scroll applies unconditionally whenever that tab is active rather than adding one just for this
(`ponytail:` comment at the call site). Playground's wheel-down/up reuse the *exact* logic
`PageDown`/`PageUp` already had, extracted into `playground_scroll_down`/`playground_scroll_up`
(`main.rs`) so the fetch-more pagination trigger (v13) and the pinned-to-bottom sentinel
normalization (v14) aren't duplicated — `handle_playground_key`'s `PageUp`/`PageDown` arms now just
call these. A new `MOUSE_SCROLL_STEP: u16 = 3` (vs. `DETAIL_SCROLL_STEP`'s 5, sized for a keyboard
page-jump) is used for the non-Playground wheel step; Playground's own wheel step stays
`DETAIL_SCROLL_STEP` since only the fetch-more/sentinel *logic* needed sharing, not the step size — a
judgment call, trivial to change if a smaller wheel step there is wanted too. `handle_mouse` gained a
`playground_tx: &mpsc::Sender<PlaygroundControl>` parameter (needed for the fetch-more trigger) — its
one call site in the event loop now threads it through, mirroring how `handle_key` already does.

**Row click-select**: new shared `App::table_pane_rect: Option<Rect>` mirrors `detail_pane_rect`
exactly (stashed every frame by each tab's own `draw()`, `None` on early-return/no-data states) —
written by Queries/Activity/Triggers (alongside their existing `detail_pane_rect`) and, new for this
tab, Tables & Indexes (which has no detail pane, so this is its first tracked `Rect`). New
`App::select_row_at(idx)` mirrors `scroll_down`/`scroll_up`'s exact bounds-check and `detail_scroll`
reset — the click counterpart of moving `j`/`k` onto a row. Row-index-from-click math
(`main.rs::row_index_at`) accounts for a fixed 2-row offset (1 border + 1 header) before the first
data row — true for all 4 tables since they all go through `theme::block()` (border on all sides) +
`.header()` with default row height 1 and no margins, verified against each table's own construction,
no per-table special-casing needed — plus the table's current scroll offset
(`ratatui::widgets::TableState::offset()`), so clicking a row that's scrolled into view still resolves
to the right data index, not just the right on-screen position.

**Tab click-switch**: new `App::tab_bar_rect: Option<Rect>`, the first *unconditionally*-every-frame
`Rect` (the tab bar is always on screen, unlike the other three which are `None` in various empty/
loading states) — written once in `ui/mod.rs::draw()` right after the top-level layout split.
`main.rs::tab_index_at` hit-tests it with an **equal-width approximation**, not an exact one:
`ratatui::widgets::Tabs` has no public API for per-segment rendered bounds (its own layout fn is
private, confirmed against vendored `ratatui-widgets` source) — a `ponytail:` comment discloses that a
click near a tab boundary can land one tab off since real titles vary in width ("Tables & Indexes" vs.
"Queries"), with summing the exact title+divider+padding widths by hand as the upgrade path if that
ever matters enough. Click action is the same `App::set_active(PanelKind::ALL[idx])` the `1`-`6` keys
already call, so tab-click and digit-key are behaviorally identical (including the `detail_scroll`
reset `set_active` already does).

Click precedence in the combined `handle_mouse_click`: tab bar, then the active tab's table (row
select), then its detail pane (zoom) — checked in that order since the three `Rect`s never overlap
(tab bar sits in its own top-level layout row, table/detail panes both live within the content row
below it), so a click matches at most one. 13 new `#[cfg(test)]` cases cover scroll/click boundary
conditions (inside/outside a pane, while zoomed, while another overlay is open, border/header rows,
out-of-range clicks, the Playground fetch-more/sentinel paths) alongside the 4 pre-existing
click-to-zoom tests, all now threading a throwaway `mpsc::channel` for `playground_tx`.

**v15**: two more Playground refinements, both direct user requests — a single unified block instead
of two separately-bordered transcript/input boxes, and Tab-triggered autocomplete for SQL keywords/
schema/table names. History (Up/Down) recall, requested in the same message, turned out to already be
fully shipped since v14 (`main.rs::playground_history_prev`/`playground_history_next`) — reverified
live after this pass, no code change needed there.

**Unified block**: `ui/playground.rs::draw()` previously rendered the transcript through
`widgets::draw_scrollable`, which wraps its content in its own bordered+titled `Block` — the live
input below it, drawn by `draw_input()`, was a bare borderless `Paragraph` with no `.block()` call at
all. So the tab was one bordered box (transcript) sitting directly above one borderless box (input), a
visible seam — not "the same like psql," a single continuous scrollback-plus-prompt view. Fixed by
extracting `draw_scrollable`'s wrap/clamp/`Paragraph`-building logic into a new
`pub(crate) fn widgets::scrollable_paragraph(lines, width, height, scroll) -> (Paragraph, clamped,
total, max_scroll)` — `draw_scrollable` itself now just calls this and wraps the result in its own
block, so its signature/behavior (and all 4 existing call sites on Queries/Triggers/Activity, plus
their tests) are completely unchanged. `ui/playground.rs::draw()` now builds one outer
`theme::block(title)` (the title carrying the same `[a-b/N]  PgUp/PgDn` scroll-position indicator
`draw_scrollable` used to show), renders it once over the full tab area, then splits its `.inner(area)`
into the transcript/input rows and renders both directly with no nested block of their own. Verified
live via a tmux-driven session against a throwaway PG14 instance — the transcript and the live input
prompt now render flush inside one border, no seam.

**Autocomplete**: new pure module `src/completion.rs` (no I/O, `#[cfg(test)]`-covered, same shape as
`diagnosis.rs`/`format.rs`) — `KEYWORDS`, a hand-rolled const list (no reserved-keyword list existed
anywhere in the codebase to reuse — confirmed by grep before writing it); `quote_ident(name)`, real
Postgres identifier quoting (`^[a-z_][a-z0-9_]*$` passes through unquoted, anything else gets
`"..."` with embedded `"` doubled) — the "valid format" part of the ask, so whatever gets inserted is
always safe to run as-is; `word_before_cursor(line, cursor_col)`, scanning backward over
identifier-or-`.` characters — a standalone char class, deliberately *not* reusing `editor.rs`'s own
private `is_word_char` (Ctrl+W's word-boundary rule shouldn't also start treating `.` as a word
character); `candidates(prefix, tables: Option<&TablesData>)`, case-insensitive prefix matching over
keywords + unqualified table names + `schema.table` for non-`public` schemas (a typed `schema.tab`
prefix is split on its last `.` and each half matched separately), capped at 50. Reuses `App.tables`
(`src/app.rs`) as-is — the slow-tier `TablesData` snapshot already populated every 5 minutes by the
*main* polling connection regardless of the active tab (see the v11 note on why Playground has its own
separate connection) — no new SQL, poll tier, or connection, and no fuzzy-matching dependency (prefix-
only, consistent with the rest of the app's naive-heuristic style).

Trigger is **Tab**, previously unbound (fell through to the catch-all no-op) — bash/readline-style
inline completion, not a DBeaver-style dropdown popup; asked the user directly which UX they wanted,
and inline-cycle was the explicit, confirmed choice (smallest diff, no new rendering surface, reuses
the existing input `Paragraph` as-is). `main.rs::playground_autocomplete()` extracts the word before
the cursor via `completion::word_before_cursor`, and either advances a cycle already in progress
(`App::playground_complete: Option<PlaygroundComplete>` — `{row, start_col, candidates, index}`,
matched on `row`+`start_col` so an edit or cursor move elsewhere starts fresh) or computes a fresh
candidate list and inserts the first match via a new `SqlEditor::replace_current_word(start_col,
replacement)` (splices `[start_col, cursor_col)` on the current line, cursor moves to the end of the
inserted text). `handle_playground_key` clears `playground_complete` on any key other than Tab (so
cycling only continues across *consecutive* Tab presses) and `on_db_switched()` resets it too (a
switched database can have a different schema, so a stale cycle shouldn't survive). Disclosed
ceilings: cycling doesn't continue past a quoted candidate (the closing `"` isn't a word character, so
the next Tab can't re-recognize the inserted text as the same word — the first completion is still
correct, only *continued* cycling on an already-quoted name is affected); column-name completion is
out of scope (no existing data source beyond the narrow FK-column list in `UnindexedForeignKey`, only
keywords/schemas/tables complete); candidates reflect the slow tier's up-to-5-minute staleness, same
as `App::tables_rates`. Verified live end-to-end via a tmux-driven session against a throwaway PG14
instance: typed `select * from us<Tab>` cycled `user_sessions` → `users`; typed `select * from
aud<Tab>` against a real mixed-case `"Audit"` schema completed to `"Audit".log` (quoted correctly, and
the query ran successfully against it); typed `sel<Tab>` completed to `SELECT`.

**Continuation-prompt follow-up** (same v15 pass, direct user report with a screenshot): a
continuation line (typed without a trailing `;`, buffered as part of the same statement — real psql's
own behavior, unchanged) used to repeat the database name on every line (`dbname-> `, matching real
psql's own `PROMPT2`). A user hit this concretely: stray text (`hi`) typed at the prompt, followed by
`enter`, buffered as a continuation rather than erroring immediately (correct, matches psql), then a
real query typed on the next line merged with it into one statement and errored on `"hi"` — confusing
specifically because the repeated `dbname-> ` prefix made the continuation line look like *another
fresh prompt*, not obviously "still part of the statement above." Fixed by dropping the repeated
database name on continuation lines — they now show a plain `-> ` instead of `dbname-> `, a deliberate
divergence from strict psql-mimicry (a direct, explicit user preference, confirmed over 2 rounds of
clarifying questions) rather than a bug in the merge-into-one-statement behavior itself, which is
unchanged and still intentional. `ui/playground.rs` gained one shared `prompt_marker(prompt, i) ->
String` (`i == 0` → `"{prompt}=> "`, else → `"-> "`) used by both `push_echo` (transcript echo) and
`draw_input` (live input) so the two can't drift apart. `draw_input`'s cursor-column math
(`prefix_width`) had been a single constant computed once per frame (`prompt.len() + 3`, correct only
because every line used to share the same prefix width) — now computed from `prompt_marker`, evaluated
against whichever row the cursor is actually on, since a continuation row's prefix is narrower than
the first row's. Verified live: typing on a continuation line and confirming the terminal cursor still
lands exactly at the end of the typed text, not offset by the old (wider) prefix assumption.

**v14**: Playground rewritten from a form ("SQL" box + "Output" box, F5-to-run) into a psql-style
REPL, on direct user feedback that the split-pane/execute-key UX "doesn't suit us" and should "follow
the same like psql does." Three structural changes: (1) **transcript, not a single result slot** —
`App::playground_result: Option<Result<...>>` (one-shot, overwritten each run) became
`App::playground_transcript: VecDeque<PlaygroundEntry>` (`{sql, result: Option<...>, has_more,
fetching_more}`, capped at `PLAYGROUND_HISTORY_CAP` same as the old flat history queue it replaces) —
every past command stays visible, scrollable, exactly like a real terminal's scrollback, rather than
being replaced by the next one. `ui/playground.rs::draw()` now renders two regions: a scrollable
transcript (region A, `widgets::draw_scrollable` — unchanged widget, full reuse) built by
`transcript_lines()` (echoes each entry's SQL with `dbname=>`/`dbname->` prompt-prefixed lines via
`push_echo()`, then its result/error/a running-spinner line), and a fixed-to-content live input area
below it (region B, `draw_input()` — deliberately *not* routed through `draw_scrollable`, since exact
non-wrapped cursor math is needed for `frame.set_cursor_position` and mixing that with wrapped
transcript text in one widget isn't reliably positionable; same "no wrap, known ceiling" tradeoff
`draw_editor` already accepted pre-rewrite). "Pinned to bottom" auto-scroll reuses `draw_scrollable`'s
own clamp-to-max behavior with zero new widget code: `handle_playground_key` sets
`app.detail_scroll = u16::MAX` on any key except PageUp/PageDown (verified against
`draw_scrollable`'s existing over-large-scroll clamp test), and PageUp normalizes that sentinel via
the previous frame's stashed `playground_max_scroll` before subtracting (`u16::MAX - step` would
otherwise still clamp straight back to the bottom next frame and never actually move). (2) **Enter
runs on `;`, not a dedicated key** — `main.rs::playground_enter` (the new plain-`Enter` handler)
inserts a newline unless `app.playground_editor.text().trim_end().ends_with(';')`, matching psql's
own completeness rule (ponytail: same naive-`;`-split disclosed limitation `split_statements`
already carries — doesn't understand a `;` inside a string/dollar-quoted literal). F5/Ctrl+Enter/
Cmd+Enter (`playground_execute`, unchanged internally) stay as a manual force-run escape hatch for
when that heuristic under/over-shoots — real psql has no equivalent, but removing the old-and-proven
override for a heuristic's rare edge case wasn't worth the regression. The confirm-guard
(`needs_confirmation`) is untouched logic-wise, just relocated: the warning now renders under the
live input (`draw_input`) instead of blanking the old separate Output pane, so prior results stay
visible while a confirmation is pending — an incidental improvement, not the point of the change.
(3) **Ctrl+C now also clears the buffer** — previously scoped to "cancel a running query" only;
`playground_cancel` gained an `else` branch (nothing running -> reset `playground_editor`/
`playground_confirm_pending`/`playground_history_cursor`) since real psql's Ctrl+C is dual-purpose
(cancel when something's running, discard a typed-into-a-corner multi-line buffer otherwise) — caught
by hand-testing the rewrite against a real psql session side by side, not the original ask, but a
direct consequence of "follow the same like psql does." Up/Down history recall
(`main.rs::playground_history_prev/next`, `App::playground_history_cursor: Option<usize>` indexing
`playground_transcript` from the front) only fires at the buffer's top/bottom row
(`editor.cursor_row() == 0` / `== lines().len() - 1`) — mid-buffer they still move the cursor, matching
real readline's own multi-line-input behavior, and needed a new `SqlEditor::set_text()` (replace the
whole buffer, cursor at end) that didn't exist before (every prior editor method mutated in place).
ponytail: unlike full readline, editing a recalled entry and then pressing Up/Down again discards
those in-place edits rather than preserving an "unsaved" slot. Result tables were also reformatted to
match psql's actual output byte-for-byte (verified against a real local `psql` session, not memory):
`|`-separated columns, a `-----+------` dashes-and-plus underline, and numeric columns right-aligned
(`ui/playground.rs::column_is_numeric` — sniffs whether every non-`NULL` value in a column parses as
`f64`, since `simple_query`'s text-only protocol doesn't expose real column type OIDs the way psql's
own type-based alignment relies on; ponytail heuristic, disclosed false-positive/negative cases in the
function's own doc) and the row-count line changed from `"N row(s)"` to psql's literal `"(N row)"`/
`"(N rows)"`. Deliberately *not* copied from real psql: NULL still renders as literal `NULL` text
(psql's actual default is a blank cell — a well-known psql footgun, indistinguishable from an empty
string; keeping it visible was a judgment call, not an oversight) and `StatementResult::Command`
still can't show the real command tag ("CREATE TABLE", "UPDATE 3") since `tokio_postgres::
SimpleQueryMessage::CommandComplete` only ever carries the trailing row-count `u64` — confirmed by
reading `tokio-postgres`'s own `extract_row_affected()`, which throws the tag text away; recovering it
would mean bypassing `Client::simple_query()` for raw protocol messages, out of scope for this pass.
Pagination (v13) needed no new mechanism, only re-scoping from flat `App` fields
(`playground_has_more`/`playground_fetching_more`) to per-`PlaygroundEntry` ones, since the
scrolled-to-bottom-of-the-transcript check is now equivalent to the old scrolled-to-bottom-of-Output
check (the paginated entry is always the last/bottommost one, by construction — pagination state is
tied to "whatever was most recently run"). `apply_event`'s `PlaygroundResult`/`PlaygroundMore`
handlers gained a defensive guard (`entry.result.is_none()` / degrade-to-no-op) against a stale result
racing a newer submission after a db switch — a real gap the old one-shot-`Option` model didn't have
to consider (see `App::on_db_switched`'s own updated doc comment for why the in-flight state is now
deliberately *not* reset on switch, unlike before). The Ctrl+H history-popup overlay
(`App::playground_history_open`, `ui/playground.rs::draw_history`) is gone entirely — superseded by
inline Up/Down recall plus natural scrollback, matching how a real terminal has no separate "history
window" either. One incidental correctness fix while rewriting the key-match anyway: the catch-all
`KeyCode::Char(c) => insert_char` arm previously had no modifier guard, so any *unbound* Ctrl+letter
combo (not one of the explicitly handled ones) would insert that letter literally into the buffer;
it's now `KeyCode::Char(c) if !modifiers.contains(CONTROL)`, with a `_ => {}` catch-all absorbing the
rest.

**v13**: a crash fix and a pagination feature, both from a single direct user report — `SELECT *
FROM <table>` on a table with one large cell panicked the whole app ("Formatting argument out of
range" at `src/ui/playground.rs:154`) and left the terminal in a corrupted state, and the user
separately asked for DBeaver-style pagination since a large result set was fetched and buffered in
full with no cap.

Root cause of the panic: `build_result_lines`' `widths` computation sized each column to the
longest cell's *byte* length (`.len()`), then `pad_row` fed that straight into a dynamic `format!`
width (`{c:<w$}`) — Rust's format machinery stores dynamic widths as a `u16` internally (confirmed
against `core::fmt::rt::Argument::from_usize`) and panics the instant any single cell exceeds 65,535
bytes (a >64KB `text`/`jsonb`/`bytea`-hex value). Fixed at the one shared point every `pad_row` call
already routes through: `ui/playground.rs::truncate_cell()` char-safely caps a cell's *displayed*
text at `CELL_MAX_CHARS` (200) with a trailing `…`, and `cell_text()`/the header line both route
through it before width is ever computed — bounding every dynamic width far below the panic
threshold regardless of what's fetched. Caught and fixed a second, related bug in the same pass while
touching this code: `format!("{:<w$}")` pads by *character* count (confirmed against
`core::fmt::Formatter::pad`), but `w` was computed from `.len()` (bytes) — any cell with multi-byte
UTF-8 already misaligned columns, independent of the panic; the width computation now uses
`.chars().count()` throughout.

Terminal corruption was a separate, related gap: `ratatui::init()` already installs a panic hook
that restores raw-mode/leaves the alt screen, but `EnableMouseCapture` and the Kitty
keyboard-enhancement flags (`main.rs`, enabled right after `init()`) were explicitly *not* wired into
that restore — a documented, deliberate ceiling from when Playground's mouse/Kitty support first
landed ("known, low-severity ceiling... not worth a custom panic hook for it"). Revisited now that a
real crash proved it matters: `main.rs` installs a second panic hook (via `std::panic::take_hook`/
`set_hook`, after `kitty_keyboard` is known) that disables mouse capture and pops the Kitty flags
(best-effort, guarded on `kitty_keyboard`) before delegating to whatever hook `ratatui::init()`
installed — cleanup-first-then-delegate, so the extra escape codes land while the alt-screen buffer
(about to be cleared) is still active, not the now-visible normal screen. Disclosed: `set_hook` is
process-global, so a panic inside a background task (`playground_task`/`poll_task`) fires this too,
mid-session, even though the app keeps running — harmless (idempotent cleanup), not fixed further.

Pagination ("scroll for more", DBeaver's own chunked-fetch UX as the named reference) is
**stateless LIMIT/OFFSET re-execution via subquery wrapping**, not a held server-side cursor —
deliberately: a held cursor needs an open transaction (or `WITH HOLD`, which still holds a
materialized result in backend memory), and an open transaction holds back the cluster-wide vacuum/
xmin horizon for *every* connection on the server, not just this one; stateless re-fetch avoids that
entirely and needs no cleanup on tab-away/db-switch/disconnect beyond what already existed. Scoped to
the common case only: `db/playground.rs::is_single_paginatable_select()` requires the submitted SQL
be *exactly one* statement (via the existing `split_statements()`) whose trimmed text is a bare
`SELECT` — deliberately **not** `WITH`, even though `is_read_only_statement()` (the older,
unrelated confirm-guard classifier) already treats `WITH` as read-only: a `WITH` query can contain a
data-modifying CTE (`WITH deleted AS (DELETE FROM foo RETURNING *) SELECT * FROM deleted`), and since
every page re-runs the *entire* inner query from scratch (see below), paginating a `WITH` would
silently re-execute a `DELETE`/`UPDATE`/`INSERT` CTE on every scroll-triggered fetch — each
`PageDown` past the first page turning into a repeated destructive side effect. Excluded statements
(multi-statement scripts, DML/DDL, `WITH`, `EXPLAIN`/`SHOW`/`TABLE`) simply keep the pre-existing
full-buffer path, now width-safe. Each page is fetched via `page_query()`:
`SELECT * FROM (<base>) AS pgpilot_page LIMIT 200 OFFSET <n>` — works uniformly for any bare `SELECT`
without parsing/rewriting its insides, and composes correctly with whatever `ORDER BY`/`LIMIT` `base`
already has (evaluated inside the subquery before this one slices it). `PLAYGROUND_PAGE_SIZE = 200`
(matches DBeaver's own default fetch size); `has_more` after any fetch is `fetched_row_count ==
PLAYGROUND_PAGE_SIZE` — fetch-*N*, not *N+1*-and-trim, since the only cost of the simpler approach is
one extra, cheap, 0-row round trip in the rare case a table's size is an exact multiple of the page
size. Known, disclosed ceilings: each page re-runs `base` from scratch (no cross-page caching, so
deep pagination on an expensive query gets progressively wasteful) and isn't snapshot-consistent
under concurrent writes between pages (no held cursor) — both fine for an ad-hoc console, not a live
consistent grid.

State lives almost entirely in `playground_task`'s own loop, not `App`: `paginated: Option<(String,
i64)>` (base query text, next offset) is local to the task, reset on every fresh `Run` and on
`SwitchDb` (a new `PlaygroundControl::FetchMore` unit variant requests the next page; if `paginated`
is `None` when one arrives — shouldn't happen in normal operation, but handled defensively — the task
still sends a `PlaygroundMore` error event rather than silently no-op'ing, so the UI's in-flight flag
doesn't get stuck forever). `App` only gained the UI-facing subset: `playground_has_more`,
`playground_fetching_more`, and `playground_max_scroll` (the last one is `widgets::draw_scrollable`'s
now-`u16`-returning max-scroll bound, stashed each frame — same "render-time geometry stashed into
`App` for the next input event" pattern `detail_pane_rect` already established in v10). The trigger
is deliberately *not* a new keybinding: `main.rs::handle_playground_key`'s existing output-pane
`PageDown` arm, after incrementing `detail_scroll`, checks whether the scroll is now at/past
`playground_max_scroll` with `has_more` set and nothing already in flight, and if so fires
`FetchMore` — the TUI analog of a GUI's "scrolled to the bottom" infinite-scroll trigger, reusing the
same key the user already presses to scroll. `AppEvent::PlaygroundResult` (existing) became a struct
variant carrying `has_more` alongside its `Result`; the new `AppEvent::PlaygroundMore` always carries
a `Result` too (not a separate error path) specifically so `apply_event` can unconditionally clear
`playground_fetching_more` from one match arm regardless of outcome. A successful `PlaygroundMore`
appends its rows into the single `StatementResult::Rows` already sitting in `app.playground_result`
via a let-chain that degrades to a safe no-op if the shape doesn't match — covers a straggling
response racing a fresh `Run` (or a `DbSwitched` reset) that already replaced `playground_result`
first; disclosed, not fixed further, since the two events come from different producer tasks on the
same channel with no ordering guarantee between them, and the worst case is one stale frame before
the reset overwrites it.

**v12**: two Playground additions, both direct user requests — cancelling a running query, and
CLI/nvim-flavored editor shortcuts. Query cancellation reuses the *existing* `PollControl::Cancel`
mechanism end-to-end rather than inventing a second cancellation path: `db::playground::backend_pid()`
runs `SELECT pg_backend_pid()` right after the Playground connection is made (and again after every
successful `SwitchDb` reconnect), broadcasting it as a new one-shot `AppEvent::PlaygroundPid(i32)` ->
`App::playground_pid`. Ctrl+C (`main.rs::playground_cancel`, gated on `app.playground_running`) sends
`PollControl::Cancel(pid)` down the *existing* `control_tx` channel to `db::poll_task` — the same
channel/handler the Activity tab's `x` key already uses — which runs `pg_cancel_backend($1)` on
*its own* client. This works specifically because `poll_task`'s client is idle for the duration of a
slow Playground query (that's the entire reason the two connections are separate — see the v11 note
below) so it's always free to fire the cancel immediately, and because both connections are opened
with the same credentials a role can always cancel its own backend without the `pg_signal_backend`
role `x`/`X` need for an arbitrary *other* session's backend. No changes to `playground_task`'s own
control loop or a new `PlaygroundControl` variant were needed — cancellation never goes through the
channel that's actually busy awaiting the long query, so there's no "the consumer is blocked in
`.await`, how does it also see a new message" problem to solve. Editor shortcuts
(`src/editor.rs::SqlEditor::{move_word_left, move_word_right, move_to_buffer_start,
move_to_buffer_end, delete_word_backward, delete_to_line_start, delete_to_line_end}`, wired in
`main.rs::handle_playground_key` as Ctrl+Left/Right, Ctrl+Home/End, Ctrl+W/U/K) are readline/
nvim-insert-mode word and line editing, **not** full vim modal editing (normal/insert/visual modes)
— the user asked for "nvim-like" shortcuts "handy for developers," and this is the non-modal subset
of that: chords, not a mode switch. Deliberately keeps `editor.rs`'s existing non-modal design intact
(see that file's own module doc, and the v11 note's `tui-textarea`/`edtui` rejection below) rather
than reopening it — going modal would mean hijacking `h`/`j`/`k`/`l`/`d`/`y`/`g` etc. as
normal-mode commands, which can't coexist with typing those same letters into SQL text without an
explicit mode toggle (`Esc`/`i`), a materially bigger redesign nothing here asked for. Word
boundaries use the simpler readline definition (a run of alnum/underscore; whitespace/punctuation are
all one separator class) rather than vim normal-mode's "punctuation is its own word" rule — good
enough for SQL identifiers, and it's what `Ctrl+W` already means in every shell. `gg`/`G` (jump to
buffer start/end) become `Ctrl+Home`/`Ctrl+End` since the letters `g`/`G` must stay typable.

**v11**: Playground — a 6th tab, and the app's first write-capable feature. Everything before this
was 100% read-only monitoring against fixed `pg_stat_*` queries; Playground is a SQL console (type,
run, and see detailed results/errors for arbitrary ad-hoc SQL against the live connection), prompted
by a direct user request with DBeaver named as the UX reference. Landed conservative in several
specific places precisely because it's the first feature that can *change* the database, while
reusing as much existing infrastructure as possible rather than inventing new mechanisms.

Multi-line SQL input is a hand-rolled buffer (`src/editor.rs::SqlEditor` — `lines: Vec<String>`,
char-index `cursor_row`/`cursor_col`, `insert_char`/`backspace`/`newline`/`move_left/right/up/down`/
`home`/`end`), not a dependency. `tui-textarea` (the standard ratatui multi-line editor crate) is
stuck on ratatui 0.29 — a real version conflict against this repo's ratatui 0.30.2 pin, not a style
preference. `edtui` does support 0.30, but it's Vim-modal (normal/insert/visual modes), a paradigm
nothing else in this app uses, and pulls in syntax-highlighting/line-number machinery disproportionate
to "type a SQL query" — both were rejected in favor of the small hand-rolled buffer, with
`Frame::set_cursor_position` (ratatui-core, confirmed against the vendored source) putting the real
terminal cursor inside it.

Execution runs on its **own dedicated `Client`/background task** (`db::playground::playground_task`,
`main.rs`'s `playground_tx`/`PlaygroundControl::{Run, SwitchDb}`), connected eagerly at startup right
next to `db::poll_task`'s own connect-and-spawn (before `conninfo`/`tls`/`conn_parts` are moved into
`poll_task`'s spawn call — `tls`/`conn_parts` are `Clone`, `conninfo` is only borrowed there). Reusing
`poll_task`'s one `Client` for a user's ad-hoc query would freeze every live panel for as long as that
query runs, defeating the app's entire premise — proven live with `SELECT pg_sleep(30)` from
Playground while confirming Overview's "updated Xs ago" kept advancing the whole time. If this second
connect fails, `App::playground_conn_error` shows a permanent in-tab error block for the session (no
retry — ponytail: restart to retry) rather than crashing. The task follows the `d`-popup database
switch (`main.rs::apply_event`'s `DbSwitched` arm now also forwards `PlaygroundControl::SwitchDb` to
it) so a query typed while looking at db "foo" can't silently execute against a stale "bar"
connection; if the playground connection's *own* reconnect fails independently of the main one's
(rare — same host/user/creds, and the main one just succeeded), it keeps running against the previous
database until the next successful switch or restart, surfaced once as a `PlaygroundResult(Err(..))`
rather than silently swallowed.

Execution itself goes through `Client::simple_query()`, not `Client::query()` — everything comes back
text-encoded (`SimpleQueryMessage::{RowDescription, Row, CommandComplete}`, confirmed
`#[non_exhaustive]` against the vendored `tokio-postgres` source), which avoids a per-Postgres-type
`FromSql` dispatch table to stringify arbitrary result columns, and natively runs `;`-separated
multi-statement scripts in one call. `db/playground.rs::group()` turns the flat message stream into
`Vec<StatementResult>` (`Rows{columns, rows}` or `Command{rows_affected}` — no command-tag text like
psql's "UPDATE 3", `simple_query` only gives a row count); split from the trivial, untested
`to_owned()` boundary specifically because `SimpleQueryRow`/`SimpleColumn` are `pub(crate)` to
tokio-postgres and can't be constructed in a test, so the actually-branchy logic sits in a pure,
fully-tested function instead. Errors reuse `db/mod.rs`'s `chained_message()` (promoted from private
to `pub(crate)` — same helper `run_signal`'s cancel/terminate path already used) for the real
server ERROR/DETAIL/HINT text, not a generic wrapper. Because it's raw SQL text, a user can type
`BEGIN; ...; ROLLBACK;` themselves for a dry run — needs no special handling.

Non-`SELECT` statements need an explicit confirm: `db/playground.rs::needs_confirmation()` splits on
top-level `;` (ponytail: naive, doesn't understand semicolons inside string/dollar-quoted literals)
and classifies each chunk read-only by a `SELECT`/`WITH`/`EXPLAIN`/`SHOW`/`TABLE` prefix check. The
execute key doubles as its own confirm — first press on an unsafe script sets
`App::playground_confirm_pending` and shows an inline warning instead of running; second press
executes; `Esc` clears it without leaving the tab; any buffer-mutating key also clears it (a stale
confirmation must not silently survive an edit to the text it was confirming) — no separate y/n key.

Key routing was the trickiest part: **the editor captures every plain key while the Playground tab is
active**, since typing SQL must not trigger the app's existing single-letter shortcuts (`1`-`5` tab
switch, `s` sort, `r` refresh, `space` pause, `q` quit, `d`/`e`/`g` popups). `main.rs::handle_key`
gained a fifth exclusive early-return branch (after the four existing ones —
`error_detail_open`/`diagnosis_open`/`detail_popup_open`/`db_popup.is_some()`, see the v10 note below)
keyed on `app.active == PanelKind::Playground`, routing to a new `handle_playground_key`. Reserved,
non-printable/modified keys carved out of the capture: **F5** (primary execute — chosen because
function keys have unambiguous escape sequences on every terminal, unlike modifier+Enter combos;
see below), **Ctrl+Enter**/**Cmd+Enter** (secondary execute aliases — `modifiers.intersects(CONTROL |
SUPER)` on `KeyCode::Enter`), **Ctrl+H** (toggle a full-screen history overlay), **Ctrl+Q**
(quit — plain `q` must stay typeable, e.g. `...FROM queue`), **Esc** (leave the tab back to Overview —
one-directional; closes the confirm-pending state or the history overlay first if either is open),
**PageUp/PageDown** (scroll the output pane, reusing the existing global binding as-is). Plain `Enter`
inserts a newline rather than executing. This required threading `KeyModifiers` into `handle_key`
(previously only `KeyCode` was passed, though `key.modifiers` was already available at the call site)
and a new `playground_tx: mpsc::Sender<PlaygroundControl>` parameter alongside the existing
`control_tx`, mirrored into `apply_event` too for the `DbSwitched`-forwarding above.

**Ctrl+Enter/Cmd+Enter fix**: originally shipped as dead code — a plain (non-enhanced) terminal
sends the identical `\r` byte for Enter and Ctrl+Enter, so crossterm always decoded both to
`KeyModifiers::NONE` and the `CONTROL`-guarded match arm could never fire; Cmd+Enter had no code
path at all. Fixed by having `main.rs::main()` negotiate crossterm's Kitty keyboard-enhancement
protocol right after `ratatui::init()`/`EnableMouseCapture`:
`crossterm::terminal::supports_keyboard_enhancement()` (a live query against the actual terminal,
not a hardcoded allowlist) gates a `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`,
popped symmetrically right before `DisableMouseCapture`/`ratatui::restore()` — same
not-wired-into-the-panic-hook ceiling `EnableMouseCapture` already carries. On a terminal that
supports the protocol (kitty, WezTerm, foot, alacritty, Ghostty, newer iTerm2), this makes
Ctrl+Enter (and Cmd+Enter, which such terminals forward as `KeyModifiers::SUPER`) actually
distinguishable from plain Enter for the first time. Disclosed ceiling: `supports_keyboard_enhancement()`
returns `Ok(false)` on Terminal.app, plain xterm, and unconditionally on Windows — those still
silently fall back to inserting a newline, exactly like before this fix — which is why F5 stays
documented as the one execute key guaranteed to work everywhere.

Results and errors render through the **existing** `ui/widgets.rs::draw_scrollable` — the same
PageUp/PageDown-scrollable, `[a-b/N]`-indicator widget Queries/Triggers/Activity's detail panes
already use (see the v9 note below) — rather than a `ratatui::widgets::Table` with row selection,
deliberately: row selection would need `j`/`k` or arrow keys, both unavailable since the editor owns
them. `ui/playground.rs::build_result_lines` formats each `StatementResult` as one lightweight
ASCII-table block (computed max-column-width padding, `NULL` for `None` cells, a trailing row count;
a `── Statement N ──` separator only appears once there's more than one, keeping the common
single-`SELECT` case clean) — no real column-resize/horizontal-scroll for very wide result sets, a
disclosed ceiling. Errors render through the same pane (red), replacing the previous result — **not**
inserted into the sticky `App::errors` map, which is reserved for a *polled panel* being persistently
broken across ticks, a different concept from a one-shot user-submitted query's error. A new
`AppEvent::PlaygroundResult(Result<Vec<StatementResult>, String>)` carries the outcome back — no new
`PanelSnapshot` variant, since Playground execution isn't part of any poll tier.

History (`App::playground_history: VecDeque<String>`, capped at `main.rs::PLAYGROUND_HISTORY_CAP =
50`) and the draft SQL itself are **in-memory/session-scoped only** — discarded on quit, same as
almost every other piece of `App` state. `config.toml` (`src/config.rs`) is deliberately **unchanged**
by this feature — worth stating explicitly since Playground is the app's first write-capable feature
and a future reader might otherwise assume persistence was added somewhere for it. `on_db_switched()`
clears `playground_result`/`playground_confirm_pending`/`playground_running` (stale execution state)
but leaves the draft text and history alone (still meaningful across a db switch in the same session).

`PanelKind::ALL` is now `[PanelKind; 6]`; `App::active_row_count()`/`scroll_down()`/`scroll_up()`/
`cycle_sort()` needed zero changes (all already end in a catch-all `_ =>` arm, so Playground
implicitly no-ops — there's no row-selectable table, and `j`/`k` never reach these methods since the
editor swallows them as literal characters first). The only *exhaustive* `match app.active` in the
codebase — `ui/mod.rs::draw()`'s tab dispatch — gained the new arm.

**v10**: mouse support — clicking the detail pane on Queries/Activity/Triggers now zooms it into a
full-screen popup (`esc`/`q` closes it, `PageUp`/`PageDown` keep scrolling the same content while
it's open). This is the first mouse interaction anywhere in the app; `ratatui::init()` doesn't
enable mouse reporting on its own, so `main.rs` now brackets the run loop with an explicit
`crossterm::execute!(stdout(), EnableMouseCapture)` right after `ratatui::init()` and
`DisableMouseCapture` right before `ratatui::restore()` — deliberately *not* wired into ratatui's
own panic-hook restore (a crash can leave the terminal in mouse-report mode until the next
`reset`/new shell; a known, low-severity ceiling, not worth a custom panic hook for). Hit-testing
needs to know each frame's actual on-screen detail-pane `Rect`, which only exists inside that tab's
own `draw()` (post-layout-split) — rather than re-deriving the layout in the input handler, `App`
gained `detail_pane_rect: Option<Rect>`, written by `queries::draw`/`activity::draw`/`triggers::draw`
right after they compute their layout split (and explicitly set back to `None` on every one of their
own early-return branches — loading/empty/not-available — so a click during those states can't match
a stale rect left over from the last successful frame). `main.rs::handle_mouse` checks
`MouseEventKind::Down(MouseButton::Left)` against that rect via `ratatui::layout::Rect::contains`,
gated the same way the three keyboard-only overlays already gate each other (ignored if
`error_detail_open`/`diagnosis_open`/`db_popup`/`detail_popup_open` — a new field, the popup's own
open flag — is already set), so clicking never fights with an overlay that's already capturing input.
The popup itself needed zero new rendering code: each tab already has a `draw_detail(frame, area,
app)` that takes an arbitrary `Rect` (built for the small inline pane, but nothing about it assumes
that), so `draw_detail_popup` is just `Clear` the full `frame.area()` then call the *same*
`draw_detail` again with that full-screen `Rect` instead — same content, same
`widgets::draw_scrollable` clamping/scrolling, no duplicated line-building logic. `ui/mod.rs::draw()`
dispatches to whichever tab's popup fn matches `app.active`, layered after the diagnosis modal, same
"popups stack on top in a fixed order" precedent as the other three. Row selection stayed
keyboard-only (`j`/`k`) — deliberately scoped to just the detail pane, since that was the specific
ask; clicking a table row does nothing. One real, disclosed tradeoff: enabling mouse capture is what
most terminals use to gate native click-drag text selection, so copying SQL text out of pgpilot via
the terminal's own selection may need a modifier-held drag (e.g. macOS Terminal/iTerm's Option-drag)
depending on the terminal — not solved here (would need a runtime toggle to suspend capture, not
requested).

**v9**: the three always-visible "detail panes" — Queries' "Selected Statement", Triggers'
"Selected Trigger", Activity's "Selected Backend" — are now scrollable (`PageUp`/`PageDown`)
instead of silently clipping long content, prompted by a direct user report of SQL text being "only
half shown" at the bottom of the screen. Root cause was two-layered: `ui/queries.rs::draw_detail`
never called `.wrap()` at all (the other two panes did), so a long single-line query was cut off
*horizontally* mid-character with no indication more text existed — the literal bug being reported —
and all three panes (including the two that did wrap) still had no vertical scroll, so a
multi-line function body or a tall stats block was clipped by the pane's fixed height regardless
(the ceiling the v7 note above originally described as permanent). Fixed with one shared
`ui/widgets.rs::draw_scrollable(frame, area, title, lines, scroll)` that all three `draw_detail` fns
call instead of hand-building their own `Paragraph`+`block` — wraps (`Wrap { trim: false }`, fixing
queries.rs's gap), clamps `scroll` to what the pane can actually show, and appends a `[a-b/N]`
position indicator to the block title only once content overflows (so overflow is visible, not
silently truncated with no affordance) — same "make broken/incomplete state visible" instinct as
`error_block`'s red-bordered "✗" treatment, applied to a different failure mode. Clamping needs a
wrapped-row count; `Paragraph::line_count()` would give an exact one but turned out to be gated
behind ratatui's `unstable-rendered-line-info` cargo feature (semver-exempt — not worth enabling for
a scroll clamp), so `widgets::wrapped_row_count()` hand-rolls a greedy width/column-division estimate
off the stable, public `Line::width()` instead — a slight undercount vs. true word-wrap on a line
that breaks early at a word boundary, immaterial here since it only feeds a clamp and an indicator,
not exact layout. `App` gained one new field, `detail_scroll: u16` (shared across all three tabs,
not per-tab, since only one detail pane is ever visible at once) — reset to 0 in `scroll_down()`/
`scroll_up()` (moving the row cursor changes the pane's content, so a stale offset shouldn't carry
over), in the new `App::set_active()` (replacing `main.rs`'s 5 direct `app.active = PanelKind::X`
assignments, so a future 6th tab can't forget the reset), and in `on_db_switched()` alongside the
other per-database state it already clears. `j`/`k` themselves are untouched — they still move the
table's row cursor exactly as before; `PageUp`/`PageDown` is new ground alongside them, not a
replacement, since the codebase had no prior "sub-focus within a tab" key-dispatch concept to
extend. Footer help text and the README keybindings table both gained a line for the new keys.

**v8**: fixed two related "cumulative/point-sampled data misread as current state" bugs, both
surfaced by the same user in one live session comparing pgpilot against real RDS CloudWatch/Console
numbers. (1) Overview's "Where Time Goes" wait-event chart (`ui/overview.rs::draw_wait_events`)
consistently showed "CPU / running" near 100% even when RDS reported the instance as nearly idle.
Root cause: `App::wait_event_counts` (a `HashMap<String, u64>`, cumulative since session start) was
only incremented for backends caught in `state = 'active'`, and the displayed percentage divided by
the sum of *those* counts — so a database that's idle 95% of the time and briefly CPU-bound the
other 5% would show "CPU / running: 100%" (100% of the *rare* active catches), which reads exactly
like sustained load even though it isn't. Point-sampling `pg_stat_activity` for wait-event
composition is itself an established technique (the `pg_wait_sampling` extension, and AWS
Performance Insights' "DB Load" chart, both do a version of this) — the bug was specifically
normalizing over active-only samples instead of every poll. Fixed by replacing the field with
`App::wait_event_samples: VecDeque<Vec<String>>` — bounded to `History::CAPACITY` (a recent window,
not cumulative-forever) and, critically, a poll that caught nothing active still pushes an *empty*
entry, so `draw_wait_events` can divide by total polls observed (not total active rows) and an
explicit `"idle (no active query)"` bucket makes the denominator visible instead of implicit. This
also makes multi-backend concurrency legible: percentages can now sum past 100% across buckets when
more than one backend was active in the same poll, an intentional "average concurrent sessions"
reading (same idea as Performance Insights' DB Load, not a bug) rather than the old scheme's forced
100%-of-active-only. (2) The v6 "disk spill (work_mem)" diagnosis suspect thresholded the *raw
cumulative* `pg_stat_database.temp_bytes` (`Warn` 1 GiB / `Bad` 10 GiB) — v6's own code comment
already flagged this as "not a live rate," and it bit within the same session: a user saw "temp
files: 790,078 (2.9 TB)" in the Buffer Cache block and asked what that was based on, correctly
suspecting it wasn't "currently happening." It's the lifetime total since the database's stats were
last reset (could be weeks), so an old one-time spill stays visible (and threshold-triggering)
forever. Fixed the same way `tps`/`rollback_pct` already handle `xact_commit`/`xact_rollback`:
`App::record_cache_overall` now computes `temp_bytes_per_sec` as a poll-to-poll delta, `None` until
the second poll. `DiagnosisInputs` gained a `temp_bytes_per_sec: Option<f64>` field (not part of
`CacheOverall` itself, same reasoning as `rollback_pct` living outside it) and the suspect/alert
text changed from "N spilled since stats reset" to "spilling N/s right now" — `Warn` at 1 MiB/s,
`Bad` at 10 MiB/s. The Buffer Cache block still shows the raw cumulative counters (`blocks hit`,
`blocks read`, lifetime temp total) for context, now under an explicit "cumulative since last stats
reset" label so they're not mistaken for live figures, plus the new rate on its own line. Both fixes
follow the same underlying lesson: a cumulative-since-X counter is a legitimate thing to *display*,
but using it directly as an alert threshold conflates "this happened at some point" with "this is
happening now" — the fix in both cases was the delta/rate pattern already established by
`record_cache_overall`'s `tps`, not a new mechanism. Full details, including why the naive
active-only normalization is a known sampling pitfall (corroborated against production monitoring
writeups on `pg_stat_activity`-based wait sampling), in `docs/postgres-incident-research.md`.

**v7**: Triggers and Activity switched from an `enter`-triggered full-screen popup for the selected
row's detail to an always-visible inline detail pane, matching `ui/queries.rs`'s pre-existing
"table on top, detail pane at the bottom, updates live as you move the cursor" shape — a direct
user request for consistent UX across the product, since Queries never needed an `enter` press to
see the selected statement's full text/stats but Triggers and Activity did. `App::trigger_detail_open`/
`App::activity_detail_open`, `ui/triggers.rs::draw_detail_popup()`, `ui/activity.rs::draw_detail_popup()`,
their `enter`-key bindings, and their overlay-check branches in `main.rs::handle_key()` and
`ui/mod.rs::draw()` are deleted, not hidden — same precedent as v4/Post-v5's full removals. Both
tabs now have a `draw_table()`/`draw_detail()` split: `ui/triggers.rs` mirrors `ui/queries.rs`
exactly (`[Min(8), Percentage(35)]`, `theme::block("Selected Trigger")` bordered with
`BORDER_DETAIL`, showing `pg_get_triggerdef` + the function's full `pg_get_functiondef` body — same
content the popup used to show, just always on screen for whichever row is highlighted).
`ui/activity.rs` gained a fifth layout row (`Length(6)`, "Selected Backend" — pid/user/state/
duration/wait + full query text) between the table and the blocking tree, shrinking the table from
`Min(8)` to `Min(6)` and the blocking tree from `Percentage(30)` to `Min(5)` to make room. At the
time neither new pane scrolled long content (it just got clipped by the pane's height) — the same
ceiling `ui/queries.rs`'s own detail pane had for a long query. v9 later fixed this across all three
panes — see the v9 Architecture note. Both detail
panes default to row 0 (`.selected().unwrap_or(0)`, not the stricter `App::selected_trigger()`/
`selected_activity_row()` helpers, which returned `None` — rendering nothing at all — until the
user pressed `j` once) so the pane is never blank on first load, matching `ui/queries.rs`'s own
`.unwrap_or(0)` exactly; those two now-unused helpers were deleted rather than left dead. This also
fixed a doc gap: `ui/activity.rs`'s bullet below never mentioned the popup it used to have at all.

**v6**: diagnosis-coverage expansion — `diagnosis.rs` went from 5 scored suspect kinds to 11 (6 new:
xid wraparound risk, connection pressure, checkpoint storm, disk spill/work_mem, replication lag,
lock wait chain) plus a 7th, alert-only N+1 query signature, prompted by a research pass into
real-world Postgres production incidents (`docs/postgres-incident-research.md` has the full
per-incident writeup with citations and thresholds). The trigger: `App`/`DiagnosisInputs` already
fetched the data behind five of these six on the existing fast/slow poll tiers for *other* panels
(`TableRow.xid_age` for the bloat suspect's text, `ConnectionsData` for the Overview meter,
`BgWriterStats` for the Checkpoints & Buffers block's own color-coding, `CacheOverall.temp_bytes`
for a stat card, `ActivityRow.blocked_by` for the Activity tab's blocking tree) — none of it was
ever thresholded or scored, so a real incident-shaped signal only surfaced as a raw number a user
had to notice themselves. `diagnosis.rs`'s own doc comment already frames this module as "derived
in-process from the other views, free"; this is that same move applied to five more views. Only
replication lag needed new SQL, and it's one column: `fetch_replication`'s query already hits
`pg_stat_replication` for byte lag, so `EXTRACT(EPOCH FROM replay_lag)` (`ReplicationRow.replay_lag_secs`)
came along in the same query rather than a new one — the far more actionable "how stale are reads"
figure vs. bytes, now also shown in `ui/overview.rs::draw_replication`'s existing text line.
`DiagnosisInputs` gained 3 fields (`connections`, `cache_checkpoints`, `replication`) wired in
`ui/overview.rs::build_inputs()`; each new `diagnose()`/`alerts()` block follows the exact same
shape as the 5 pre-existing ones (a threshold const pair, a `max_by`/filter over already-borrowed
data, a `Candidate`/`Alert` push) — no new module, no new poll tier, no `PanelSnapshot` changes.
`Severity` now derives `Ord` (`Warn < Bad` by declaration order) so `alerts()` sorts worst-first
before truncating — with 6 new alert sources appended after the original 4, the old fixed-order
`truncate(5)` could silently starve a real new high-severity alert behind older lower-priority ones
that happened to be checked first; the cap also moved to 8 (`alerts()` and the modal's own
`alerts.iter().take(n)` in `ui/overview.rs::draw_diagnosis_modal`, which must stay in sync — it
reads its own slice length, not `alerts()`'s truncated size). N+1 detection (high `calls`, ~1
row/call, low `mean_exec_time_ms` in `pg_stat_statements`) is deliberately alert-only, not a ranked
suspect — it's the fuzziest heuristic of the seven, same treatment already given to unused-indexes
and cache-hit-ratio. Temp-file spill's `temp_bytes` threshold is cumulative since the last stats
reset, not a live rate — same simplification class `App::wait_event_counts` already documents;
noted with a `ponytail:`-style comment in `diagnosis.rs` rather than silently assumed. Lock wait
chain detection is one level deep, matching the Activity tab's own blocking-tree ceiling, not a new
limitation. Two new `#[cfg(test)]` cases cover the two least-obvious heuristics (xid age crossing
the bad threshold, the N+1 signature); the other four are direct threshold comparisons on data
shapes the existing tests already exercise.

**Post-v5**: Overview's "Per Database" cache-hit-ratio block was deleted — it duplicated the `cache hit` column `ui/picker.rs` already shows per database (`DatabaseRow.cache_hit_pct`, `src/db/databases.rs`), same metric, one popup away via `d`. Removed end-to-end, not just hidden: `ui/overview.rs`'s `draw_per_database()` and its layout slot (the cache blocks row is now `[50%, 50%]` — Buffer Cache detail / Coldest Relations, was `[34%, 33%, 33%]` with Per Database in the middle), `db/cache_io.rs`'s `fetch_per_database()`/`PER_DATABASE_QUERY`/`CacheDbRow`, `PanelSnapshot::CachePerDatabase`, `App::cache_per_database`, and its slot in `db/mod.rs::send_fast`'s fast-tier array (now 8 queries, was 9). Same precedent as v4's deleted cache-hit meter — don't keep two renderings of one number.

**v5**: per-query disk I/O on the Queries tab — the one thing RDS Performance Insights shows that pgpilot had no equivalent for, prompted by a real "a query is hammering I/O and this dashboard shows it nowhere" report. The gap was three-deep and fixing only the visible layer wouldn't have worked: the SQL fetched no write/temp/timing columns, the top-100 row cut was ordered by `total_exec_time` alone (so a disk-heavy, wall-clock-cheap query never even arrived), and the sort cycle had no I/O key. All three fixed — see `db/statements.rs`, `app.rs`'s `QueriesSortColumn`, and `ui/queries.rs` below. Four files, no new module/panel/poll tier: the medium tier already fetches `pg_stat_statements`, so this is more columns on the query that was already running, not a new one. Verified live against throwaway PG17.10 (`track_io_timing = on`) and PG14.23 (off) clusters — both branches of the renamed timing columns, and the timing-off `—` + hint path.

**v4**: tab consolidation. The `CacheIo` tab is gone — `PanelKind` shrank from 6 variants to 5 (`Overview, Queries, Activity, TablesIndexes, Triggers`; key `4`/`5` now land on `TablesIndexes`/`Triggers`, footer help updated to `1-5`) — and its 5 blocks (Buffer Cache detail, Per Database, Coldest Relations, Checkpoints, Replication) now render on Overview instead, since Overview was the thinnest tab (3 blocks) while Cache & I/O was actually the fullest (5). `src/ui/cache_io.rs` was deleted; its 5 draw fns moved into `src/ui/overview.rs` verbatim (same `source_label` strings, same `App` fields — no `db/mod.rs`/`PanelSnapshot` changes, this was a UI-layer-only move) rather than kept as a separate module, since a 175-line file with no independent tab identity left was more confusing to keep around than to fold in. Overview's own cards row lost its "cache hit" meter (`draw_meter`, and `cache_color()` was deleted with it) since it plotted the exact same `app.history.cache_pct` series as the newly-arrived Buffer Cache block's hit-ratio+sparkline — keeping both was showing the same number twice. The "Checkpoints & WAL" block was renamed to "Checkpoints & Buffers" and restyled: it never actually carried WAL data (no `pg_stat_wal` query exists anywhere in this codebase — confirmed by reading `src/db/cache_io.rs`, which only fetches `pg_stat_bgwriter`/`pg_stat_checkpointer` counters), so the old name was aspirational rather than accurate. The redesign replaced one flat, single-color `Paragraph` line with per-field coloring (the existing `checkpoints_req > checkpoints_timed` heuristic now colors just its own line via a `charts::bar()` timed-vs-requested ratio, and a new `maxwritten_clean > 0` threshold independently colors the buffers-written line) — still no new SQL, no new history series, just presenting fields that were already being fetched more legibly. `db/cache_io.rs` (the fetch layer: `fetch_overall`/`fetch_per_database`/`fetch_coldest`/`fetch_bgwriter`/`fetch_replication`) is untouched by this — only the UI-layer module moved.

**v3**: a UI simplification pass, catching up to the same `Postgres Monitor TUI.dc.html` design mockup after the user revised it in the design tool post-v2 — the file's content changed (fetched fresh via `DesignSync.get_file`, diffed against what v2 had built), not a re-read of the original. Two changes: (1) `ui/theme.rs`'s palette went from the dark-green-tinted theme to a monochrome grey/white one (`BG`/`PANEL_BG`/`PANEL_BG_ALT` now `#0a0b0a`, `TEXT` `#d6d8d6`, `TEXT_BRIGHT` `#ffffff`, `TEXT_DIM`/`TEXT_DIMMER`/`TEXT_DIMMEST` `#7a7d7a`/`#5c5f5c`/`#4a4d4a`, `BORDER` `#232523`; `OK`/`WARN`/`BAD` stay green/amber/red but are now the *only* colored elements against an otherwise grey UI). `BORDER_DIAGNOSIS` and `BORDER_WARN` were deleted (their one call site each either went away with the diagnosis strip or got repointed to `BORDER_DETAIL`, now the single shared emphasis-border color for any modal/notice box regardless of what triggered it) — `ROW_SELECTED_BG` is now just an alias for `BORDER` rather than its own hex, since the new palette doesn't need a distinct selection tint. (2) Overview's always-visible "Diagnosis" strip and "Needs Attention" alerts list are gone — replaced by a `g`-triggered full-screen modal (`overview::draw_diagnosis_modal`) plus a small severity-colored badge in the tab bar so the at-a-glance signal survives even though the detail doesn't take up permanent screen space. `App::diagnosis_open: bool` follows the exact same toggle-overlay pattern as `error_detail_open`/`trigger_detail_open` (reset in `on_db_switched()`, exclusive early-return branch in `handle_key()`, always-openable — no guard condition, unlike the other two, since the diagnosis heuristic is always computable even with zero suspects). `diagnosis.rs` itself is untouched — `diagnose()`/`alerts()` are computed exactly once per frame in `ui/mod.rs::draw()` (via `overview::build_inputs()`, now `pub(crate)`) and the owned results passed down to both the tab-bar badge and the modal, rather than each recomputing the heuristic independently. The Overview stat-card row also shrank from 4 full cards to 2 (transactions/s, latency p95 — both keep their sparkline) plus a narrow side column of compact one-line "meter" bars for connections/cache-hit (`overview::draw_meter`, reusing the existing `charts::bar()`); the old separate wide throughput area-chart panel was dropped as redundant with the tps card's own sparkline, and `charts::area_chart()` was deleted along with it since nothing else called it. The Triggers tab (6th tab, not part of either mockup revision) was explicitly kept as-is — real functionality, not being cut for the sake of matching the mockup. Every other tab (Queries, Activity, Cache & I/O, Tables & Indexes) only picked up the new palette, no layout changes, since their content already matched the revised mockup.

**v2**: a from-scratch UI/feature expansion implementing the `Postgres Monitor TUI.dc.html` design mockup (`claude_design` MCP, project `PostgreSQL Performance Monitor TUI`, id `ce4aac14-ae5c-4fda-9e54-dfd87abae33a` — re-fetch via `DesignSync.get_file` for exact palette/column-width/glyph specifics if extending a tab). v1's four panels (connections, cache hit, index health, table sizes) have been folded into five new tabs — Overview, Queries, Activity, Cache & I/O, Tables & Indexes — restyled in the mockup's dark green/JetBrains-Mono terminal theme (colors don't matter for a terminal font choice, but the palette does — centralized in `ui/theme.rs`). Kept `tokio-postgres`/`clap`/`ratatui` throughout — the mockup project's own `rust-build-notes.md` suggested a sync-Postgres/hand-rolled-args rewrite for binary size, deliberately **not** followed, since rewriting a working, documented async app for a size optimization nobody asked for is out of scope. Brand text stays "pgpilot" in the UI, not the mockup's "pgtop" — the project was already deliberately renamed once (see git history / CLAUDE.md's project-rename note). The database-picker screen now *does* also show at launch (this reverses an earlier decision recorded here that called it redundant — the user explicitly asked for it), and — a second explicit request — no panel is polled from *any* database until the user resolves that picker: `main.rs` fetches `db::databases::fetch(&client)` once (the picker's own list, not dashboard data), populates `App::databases`, calls `App::open_db_popup()`, then blocks in `run_startup_picker()` (a small dedicated `j`/`k`/enter/esc/q/d loop, reusing `ui::draw`/`App::popup_*` — same keys as the `d` popup) until the user confirms a selection — `db::poll_task` isn't spawned until that function returns, so nothing is fetched in the background while the picker is up (verified live: `pg_stat_activity` showed the app's one backend sitting idle on exactly the one `databases::fetch` query for as long as the picker went untouched). If the chosen db differs from the one already connected, `main.rs` rebuilds the conninfo via `cli::build_conninfo()` against `ConnParts` and reconnects right there (mirroring, but not sharing code with, `db::poll_task`'s own `SwitchDb` reconnect) before finally spawning `poll_task` and entering the real event loop. `onboarding.rs`'s pre-connection profile picker still resolves which *server/user* to connect to first; this popup layers on top of that to let the user confirm or change the *database* before any polling — or the dashboard itself — starts.

- `src/main.rs` — entry point. Connects once up front via `connect_with_spinner()` (fails fast with a clean stderr message before entering the alt screen; animated Braille spinner via `tokio::select!` between the connect future and an 80ms ticker — ordinary `print!`+`\r`, not a ratatui widget, since it runs before `ratatui::init()`). When `can_switch_db`, does one `db::databases::fetch(&client)` right here (before the client is moved into `poll_task`) to populate `App::databases`, calls `App::open_db_popup()`, then awaits `run_startup_picker()` — its own tiny `tokio::select!` loop (crossterm `EventStream` + a 250ms render tick, same shape as `run()` below but scoped to just `j`/`k`/enter/esc/q/d) that blocks the dashboard from ever starting until the user resolves the picker. Only once that returns does it (optionally reconnect, if the chosen db differs, then) spawn `db::poll_task` — see the v2 Overview note above for why this ordering matters (no per-database polling before the user picks). Then hands that `Client` to the spawned `db::poll_task` along with a shared `Arc<Notify>` (the `r`-key manual-refresh trigger, unchanged since v1) and an `mpsc::Sender<PollControl>`/`Receiver<PollControl>` pair (`control_tx`/`control_rx` — see `db::PollControl` below), then runs the merged event loop: `tokio::select!` over crossterm's `EventStream` (keys), the `mpsc::Receiver<AppEvent>` (DB snapshots/errors), and a 250ms render-tick interval, redrawing after every iteration. `ratatui::init()`/`ratatui::restore()` handle panic-safe terminal setup/teardown. `handle_key()` checks the exclusive full-screen overlays first, in order — `error_detail_open`, `diagnosis_open` — each closing on its own small key set (`esc`/`q`/`e`, `esc`/`q`/`g` respectively) before falling through; then `app.db_popup.is_some()` (picker open: `j`/`k`/arrows move selection, `enter` sends `PollControl::SwitchDb` unless `DatabaseRow::unswitchable_reason()` says otherwise — then it's just a status message, no connection attempted — `esc`/`q`/`d` closes without switching); otherwise: `1`-`6` select tab, `d` opens the picker, `g` opens the diagnosis modal (no guard — always openable), `j`/`k`/arrows scroll/select the active tab's rows, `s` cycles sort, `r` notifies `refresh_now`, `space` toggles `app.paused` (flipped locally *and* sent as `PollControl::TogglePause` — don't wait on the round trip for header feedback), `-`/`+` cycle `RATE_OPTIONS_MS` via `bump_rate()` (sends `PollControl::SetInterval`), `x`/`X` send `PollControl::Cancel`/`Terminate` for `App::selected_activity_pid()` when the Activity tab is active. `apply_event()` routes each `PanelSnapshot` to the matching `App::record_*()` method (these compute rates/history from the *previous* stored value before it's overwritten — see `app.rs`) or a plain field assignment for panels with no derived rate.
- `src/event.rs` — `PanelSnapshot` (one variant per polled panel: `Connections`, `CacheIo`, `Indexes`, `UnindexedForeignKeys`, `Tables`, `Databases`, `Statements`, `Activity`, `Triggers`), each with a `source_label()` matching the label `db::send_labeled` used for that panel's fetch (see below), and `AppEvent` (`Snapshot(PanelSnapshot) | Error{source, message} | DbSwitched(String) | ServerInfo(ServerInfo) | Status(String, StatusLevel)`). `ServerInfo` is its own event, not a `PanelSnapshot` — it's fetched once at connect/reconnect/db-switch, not on any polling tier (nothing in it changes mid-session except db size, which *is* on the fast tier via `Databases`). `Status`/`StatusLevel` back the footer's transient status line (sort changed, refreshed, cancel/terminate result) — distinct from the sticky per-source `errors` map (`App::errors`, see below).
- `src/app.rs` — `App` struct, the biggest file. `PanelKind` (`Overview | Queries | Activity | TablesIndexes | Triggers`, `ALL` const array, `title()`). Per-tab data: most fields are plain `Option<T>` (`connections`, `indexes`, `unindexed_fks`, `activity`, `cache_per_database`, `cache_coldest`, `cache_checkpoints`, `cache_replication`), but ones a block needs a *rate* from are `Option<(T, Instant)>` (`cache_overall`, `tables`, `databases`, `statements`) — pairing value+timestamp reuses `App`'s existing "one generation back" storage for delta computation instead of a separate previous-snapshot cache. Overview's cache/checkpoint blocks (`cache_overall`/`cache_per_database`/`cache_coldest`/`cache_checkpoints`/`cache_replication` — 5 fields, since `_overall` also carries throughput history) are separate fields rather than one `cache_io` struct specifically so each can independently be `None`/erroring without the others being affected — see `db::cache_io`'s per-fetch split below. `History` (5-6 `VecDeque<f64>`, capacity 120, `push`/`pop_front`) backs the Overview sparklines — each series is pushed on *its own* source tier's cadence (fast-tier metrics sample often; `p95`, sourced from `pg_stat_statements`, only every medium tick), so series can hold different sample counts over the same wall-clock span; that's expected. `wait_event_samples: VecDeque<Vec<String>>` is a bounded (`History::CAPACITY`, ~120 polls) rolling window of per-poll wait-event snapshots — since v8 replacing an unbounded, cumulative-since-session-start `HashMap<String, u64>` that normalized only over active-caught polls (see the v8 Architecture note for why that overstated load); still point-sampled, not fully time-integrated the way `pg_wait_sampling` would be, a known remaining ceiling. `tables_rates: HashMap<(schema,table), TableRowRate>` holds derived seq-scans/hour, computed once per slow-tier snapshot (`App::record_tables`) rather than embedding a rate field in the pure-fetch `db::tables::TableRow`. Sort/scroll: `queries_state`/`queries_sort` (`QueriesSortColumn`, 4-way cycle since v5 — total time → mean time → calls → disk I/O — always descending; the I/O key is `StatementRow::io_blocks()`, not `io_time_ms`, since blocks are always populated while timing is 0 unless `track_io_timing = on`, marked with a `ponytail:` comment), `activity_state` (+ `selected_activity_pid()`, used by `x`/`X` cancel/terminate), `tables_state`/`tables_sort`/`tables_sort_dir` (same size/name toggle-with-reversal as v1), `triggers_state` (no sort — a small static slow-tier list, `cycle_sort()`'s `_ => {}` fallthrough already covers it, same as Overview/Activity) — `active_row_count()`/`scroll_down()`/`scroll_up()`/`cycle_sort()` dispatch on `app.active`. Runtime knobs: `paused: bool`, `rate: Duration` (mirrors `poll_task`'s fast-tier interval for header display), `ascii: bool`. Database picker: `databases`/`databases_prev: Option<(Vec<DatabaseRow>, Instant)>` (the `_prev` pair lets `ui/picker.rs` compute per-db tps at render time), `current_dbname`, `server_addr: Option<String>` (`host:port` for the header; `None` for `--dsn`), `can_switch_db`, `db_popup: Option<usize>` with `open_db_popup()`/`close_db_popup()`/`popup_move()`/`popup_selected_database() -> Option<&DatabaseRow>`. `on_db_switched()` resets all per-database state (not the database list itself, which is cluster-wide). **Errors**: `errors: BTreeMap<String, String>` (source label -> message), *not* a single `Option<String>` — each poll cycle can now have some panels fail and others succeed independently (see `db::send_fast` below), so a source's entry is only cleared when *that same source* succeeds again (`apply_event` in `main.rs` does `app.errors.remove(snapshot.source_label())` on every `Snapshot`, never a blanket clear — an earlier blanket-clear version of this made a persistently-failing panel's error flash and vanish the instant any unrelated panel's next poll succeeded, since `Snapshot` events and `Error` events interleave within one poll cycle). `error_detail_open: bool` (toggled by `e`, only reachable when `errors` is non-empty) opens a full-screen overlay (`widgets::draw_error_detail`) showing every current error's full, unwrapped text — the footer's one line necessarily truncates long Postgres error messages. `diagnosis_open: bool` is the same overlay shape again, toggled by `g`, opening `overview::draw_diagnosis_modal` — unlike `error_detail_open` it has no reachability guard, since the diagnosis heuristic always produces a result (possibly empty) rather than depending on a non-empty error map. Triggers and Activity used to have their own `enter`-triggered popups the same shape as `error_detail_open` (`trigger_detail_open`/`activity_detail_open`) — removed in v7 in favor of an always-visible detail pane, see below.
- `src/diagnosis.rs` — pure, sync, no SQL of its own: `diagnose(&DiagnosisInputs) -> Diagnosis` (a headline + up to 4 ranked `Suspect`s) and `alerts(&DiagnosisInputs) -> Vec<Alert>` (a fix-hint list), both reading whatever's already sitting in `App`'s already-fetched panel fields (`DiagnosisInputs` borrows them explicitly rather than taking `&App`, so this module has zero dependency on `App`'s shape). Since v3, both are computed exactly once per frame in `ui/mod.rs::draw()` rather than independently inside `overview.rs` — the owned `Diagnosis`/`Vec<Alert>` results get passed down to the tab-bar badge and the `g`-triggered modal (`overview::draw_diagnosis_modal`), which is the only place `Alert.text`/`.hint`/`.severity` and `Suspect.evidence` still render; this file itself needed zero changes for that move. Ranking is heuristic — candidate kinds (slow query, idle-in-transaction, bloat, unindexed FK, seq-scan-heavy table) are scored on different, incomparable bases and just sorted by that score; good enough to point at the right haystack, not a precise db-time cost model (the mockup's suspects table has a "share of db time %" column with no backing data here — deliberately dropped from the modal rather than fabricated). `#[cfg(test)]` covers the seq-scan-heavy heuristic and the empty-inputs case.
- `src/format.rs` — `human_bytes(i64)`, `human_duration(f64_secs)`, `human_rate(Duration)`: hand-rolled coarsest-readable-unit formatting (no external crate), shared by the db layer's `EXTRACT(EPOCH ...)` seconds and the UI's poll-rate display.
- `src/ui/theme.rs` — `pub const` `Color::Rgb` palette values (bg/panel/border tiers, text tiers, OK/WARN/BAD, row-selected). As of v3 this is a monochrome grey/white palette (`BG`/`PANEL_BG`/`PANEL_BG_ALT` `#0a0b0a`, `TEXT` `#d6d8d6`, `TEXT_BRIGHT` `#ffffff`, `TEXT_DIM`/`TEXT_DIMMER`/`TEXT_DIMMEST` `#7a7d7a`/`#5c5f5c`/`#4a4d4a`, `BORDER` `#232523`, `ROW_SELECTED_BG` now just an alias for `BORDER`) — the earlier v2 palette was dark-green-tinted throughout; `OK`/`WARN`/`BAD` are the only colors that still carry semantic hue, everything else is grey. `BORDER_DETAIL` (`#3a3d3a`) is the single shared emphasis-border color for any modal or notice box (error detail, trigger-function popup, the diagnosis modal, the pg_stat_statements-not-loaded notice) regardless of what triggered it — `BORDER_DIAGNOSIS` and `BORDER_WARN` (formerly separate tinted borders for the diagnosis strip and the pg_stat_statements notice) were deleted when v3 consolidated onto this one constant. Also exports `block(title) -> Block<'static>`, the one shared constructor every tab uses for its bordered panels: **`Block::title()` with a plain string silently inherits `border_style`'s color when no title style is set** (confirmed via a `TestBackend` render — title cells came back with `fg` equal to the border color, not `Color::Reset`), so a caller that builds `Block::default().title(title).border_style(...)` by hand without an explicit title style gets a title in whatever near-invisible border shade got used — this bit real UI titles before `theme::block()` existed, since `BORDER` sits only ~2 RGB shades above `PANEL_BG`. `theme::block()` always sets the title's `Span` style explicitly (`TEXT_DIM` + bold — grey enough not to compete with bright values, bold enough to read as a title against normal body text), overriding that inheritance. Any new bordered block should go through `theme::block()` (chain `.border_style(...)` after it for the few blocks — Queries' extension notice, Selected Statement, the diagnosis modal — that intentionally use `BORDER_DETAIL` instead of the default) rather than hand-rolling `Block::default()...` again.
- `src/ui/charts.rs` — pure string-building: `sparkline()`, `delta_arrow()`, `bar()`, plus `glyphs(ascii: bool) -> Glyphs` (cursor/pause/dot/root/branch/full/empty/up/down — the ascii-vs-Unicode glyph pairs, e.g. `›`/`>`, `❚❚`/`||`, `└─`/` \_`). Custom renderer, not `ratatui::widgets::Sparkline` — that widget has no ASCII fallback and isn't meant to sit inline next to a stat-card value. `#[cfg(test)]` covers flat/ramp sparkline input, ascii-vs-unicode divergence, bar fill, and delta-arrow direction.
- `src/ui/mod.rs` — `draw(frame, &mut App)`: 4-row vertical layout (header / tab bar / content `Min(0)` / footer). Computes `diagnosis::diagnose()`/`alerts()` once per frame (via `overview::build_inputs()`) before the tab dispatch, scoping the `&App` borrow those need to a small block so the owned `Diagnosis`/`Vec<Alert>` results outlive it without fighting the `&mut App` the per-tab draws need for their own stateful widgets — passes the `Diagnosis` into `widgets::draw_tab_bar()` for the badge, then dispatches to the active tab's module, then layers popups on top in order: `ui::picker::draw()` if `app.db_popup.is_some()`, `widgets::draw_error_detail()`, and `overview::draw_diagnosis_modal()` if `app.diagnosis_open`.
- `src/ui/widgets.rs` — `spinner()`/`loading()` (unchanged 10-frame Braille spinner, shared with `main.rs`'s pre-TUI connect spinner), `draw_header()` (brand, current db, `host:port`, PG version, db size, owner, sample-rate note, uptime, live/paused indicator — reads `App::server_addr`/`server_info`/`databases`), `draw_tab_bar(frame, area, active, diag: &diagnosis::Diagnosis)` (restyled `ratatui::widgets::Tabs`, `PANEL_BG_ALT` background, plus — since v3 — a right-aligned severity badge painted over the same block: dim "no issues" when `diag.suspects` is empty, else a colored issue count, `BAD` if any suspect is `Severity::Bad` else `WARN`), `draw_footer()` (help text — including the always-on `g: diagnose` hint — or `app.status` if set, or an `app.errors` summary in red — all with the `updated {N}s ago` freshness suffix). **`error_block()`/`loading_or_error()`**: every block on every tab calls `loading_or_error(frame, area, app, source_label, title)` instead of bare `loading()` for its "no data yet" state — it renders `error_block` (a red-bordered `✗ <title>` block with that source's message + an `(e: full error)` hint) when `app.errors` has an entry for that block's `source_label`, or the normal spinner otherwise. This means a broken block reads as *broken*, not as stuck loading forever, and — combined with the per-query isolation above — each block fails independently and visibly rather than the whole tab going dark. `draw_error_detail()` (full-screen, `e`/`esc`/`q` to close) lists every current error as a numbered point (source, then its full message indented below) rather than a wall of text.
- `src/ui/overview.rs` — since v4, a 4-row layout: `draw_cards` (2 stat cards — transactions/s, latency p95, both via `render_card`: delta arrow vs. 6-samples-ago + sparkline, generic over unit/format-fn — plus a narrow side column with a single one-line `draw_meter()` bar for connections, reusing `charts::bar()`; the cache-hit meter that used to sit alongside it was deleted in v4 as a duplicate of the Buffer Cache block below), then a `[60%, 40%]` row of a top-5-statements mini table and a wait-events bar list (`draw_wait_events`, reading `app.wait_event_samples` — since v8 normalized over every poll in the window, not just the ones that caught something active, with an explicit `"idle (no active query)"` bucket), then the cache/checkpoint blocks absorbed from the deleted `CacheIo` tab: a `[34%, 33%, 33%]` row of Buffer Cache detail / Per Database / Coldest Relations, then a `[60%, 40%]` row of Checkpoints & Buffers (renamed from "Checkpoints & WAL" — no WAL data was ever fetched, see v4 note above) / Replication. The always-visible diagnosis strip and alerts list from v2 are gone from this layout — `build_inputs()` (now `pub(crate)`) and `pub fn draw_diagnosis_modal(frame, diag: &Diagnosis, alerts: &[Alert])` still live here (Overview-tab-specific content), but the modal itself is called from `ui/mod.rs` only when `app.diagnosis_open`, with `diag`/`alerts` computed there once per frame rather than by this module reaching into `&App` itself — the modal renders the headline, ranked suspects (rank/kind/text/evidence, no share-of-db-time column — no backing data for one), then a "Fixes" section listing each alert's `text`/`hint` colored by `severity`. Reads `&App` only in its per-tab `draw()` — no tab here owns row-selection state.
- `src/ui/queries.rs` — `pg_stat_statements` table (sortable, row-selectable) + a detail pane for the selected statement (full SQL, stat breakdown, a small rule-based `advise()` heuristic) when `Available`. Since v5 the table carries an `io` column (`format::human_bytes(row.io_bytes())`) and an `io ms` column, and the detail pane breaks out shared reads/writes, temp-file spill, and disk time. The `io` cell is colored by **share of the fetched set's total** disk traffic (≥25% `BAD`, ≥10% `WARN`) with a 64 MB floor, not by an absolute byte threshold — any fixed MB cutoff is wrong at some workload size, and the floor is what stops an idle database from painting its one trivial reader red at 100% share. `io_time_ms == 0.0` renders `—` rather than `0`, because zero there means `track_io_timing = off` (the default), not "no disk time"; when *every* fetched row is zero the detail pane appends one dim `SET track_io_timing = on` hint — an all-rows-zero check, not a second `SHOW` query. `advise()`'s first two branches are I/O-first (temp spill → `work_mem`; `shared_blks_read > shared_blks_hit` → missing index / table outgrew `shared_buffers`) ahead of the older cache-hit/latency/variance ones; a clear "extension not loaded, here's how to enable it" notice when `NotAvailable` — checked via a `matches!` on `&app.statements` *before* taking `&mut App` for the table (avoids a borrow-checker fight between reading the enum and mutating `queries_state`).
- `src/ui/activity.rs` — 5 connection-state summary cards (sourced from `app.connections`, the same aggregate query the old Connections panel used — not re-derived by scanning every activity row), the `pg_stat_activity` table (row-selectable, `x`/`X`-targetable), a "Selected Backend" detail pane (pid/user/state/duration/wait event + full query text for whichever row is highlighted — since v7, replacing an `enter`-triggered popup, same inline-pane shape as `ui/queries.rs`), a blocking tree (built from `ActivityRow::blocked_by`, root blockers first then their direct waiters — one level deep, not a full recursive chain), and a lock/txn summary strip.
- `src/ui/tables_indexes.rs` — schema-totals strip (restored from v1's Table Sizes panel), the extended tables table (dead-tuple %, xid age, seq-scans/hour from `app.tables_rates`, last-vacuum age; reuses `tables_state`/`tables_sort`), an "Unused / Invalid Indexes" list (zero-`idx_scan` **and** invalid/not-ready indexes — folds in what was v1's Index Health color-coding — with summed reclaimable size, capped at 20 rows, no scroll state), and a "Missing Index Candidates" list (`missing_index_candidates()`: unindexed FKs from `app.unindexed_fks` + high-seq-scan-ratio tables — deliberately scoped down from the mockup's fabricated column-level `CREATE INDEX (col1, col2)` guesses, which aren't realistically derivable without query-plan analysis).
- `src/ui/triggers.rs` — 6th tab, added after the mockup-driven five above. A row-selectable table of every user-defined trigger (schema/table/trigger/function/enabled state, `triggers_state`, no sort — see `App` above) plus — since v7 — an always-visible "Selected Trigger" detail pane below it (same `[Min(8), Percentage(35)]` `draw_table`/`draw_detail` split as `ui/queries.rs`) showing the selected trigger's `pg_get_triggerdef` DDL and its function's full `pg_get_functiondef` body — the actual "query" that runs when the trigger fires. Previously an `enter`-triggered full-screen popup (`draw_detail_popup()`/`app.trigger_detail_open`); removed in favor of updating live with the row cursor, no key press needed, matching how every other tab's detail already worked.
- `src/ui/picker.rs` — full-screen (not the old centered-overlay popup) `Table` over `frame.area()` listing `owner`/`size`/`sessions`/`tps`/`cache hit`/`state` per database; `tps_deltas()` computes per-db commit+rollback rate from `app.databases` vs `app.databases_prev`. Templates are visually dimmed and rejected on `enter` (status message, no switch attempt).
- `src/cli.rs` — unchanged from v1 except one addition: `--ascii` bool flag, read into `App::ascii` at startup. (`Cli`, `ConnParts`, `build_conninfo()`/`quote_conninfo_value()` — see the quoting note below, still load-bearing for any new conninfo-building code.)
- `src/config.rs`, `src/onboarding.rs`, `src/db/tls.rs` — **untouched by v2.** Saved-profile CRUD, the pre-connection picker/wizard, and TLS all work exactly as documented below (v1 section folded up here since nothing changed): `Profile{host,port,user,dbname,ssl,ssl_root_cert,ssl_client_cert,ssl_client_key}` (no password field by design), `Config` load/save via `directories::ProjectDirs::from("", "", "pgpilot")` + `toml` (`~/Library/Application Support/pgpilot/config.toml` on macOS, `0600`); onboarding prompts run before `ratatui::init()` (plain stdin/stdout, not a ratatui form); `TlsMode::{Disabled, Enabled{root_cert,client_cert,client_key}}` + `make_connector()` (rustls, accept-any-cert when no CA given, mutual TLS when both client cert+key given).
- `src/db/mod.rs` — `connect()` unchanged (spawns the `Connection` driver task — required, or `Client` silently dies). `PollControl` (`SwitchDb(String) | SetInterval(Duration) | TogglePause | Cancel(i32) | Terminate(i32)`) replaces v1's bare `mpsc::Receiver<String>` switch-db channel. `poll_task()` now runs **three tiers**, each its own `tokio::time::Interval` with `MissedTickBehavior::Delay` (so resume-from-pause doesn't tick-burst): fast (starts at `--interval`, the only tier `-`/`+` touches — connections/cache-io/activity/indexes/databases, via `send_fast`), medium (fixed 15s — `pg_stat_statements`, via `send_medium`, skipped with zero queries if the cached extension-presence bool is false; takes `pg17_plus` as of v5, same `poll_task` local `send_fast` already used, for the renamed I/O-timing columns), slow (fixed 5min — catalog-heavy tables+unindexed-FKs+triggers, via `send_slow`). `paused` gates all three tiers' dispatch but not the `refresh_now` `Notify` (an explicit `r` always works) nor the control channel.

  **Per-query failure isolation** (`send_labeled()`): each tier's queries are sent *independently* through `send_labeled(tx, label, result)`, not bundled into one `?`-chained fetch that aborts on the first error — bundling meant one version-sensitive query failing (e.g. `pg_stat_bgwriter`'s columns differing on PG17+) silently blocked every *other* query in that tier from ever populating, since they shared one fallible function. Hit this exact bug live: a broken `cache_io` query on the fast tier was blocking Connections/Activity/Indexes/Databases too, and by extension Overview (which reads `app.connections`/`app.activity`). `send_labeled` returns `(channel_still_open, connection_looks_dead)` — the latter only `true` when the underlying `tokio_postgres::Error::is_closed()` says the *connection* itself died, not for an ordinary query-level failure, so a broken query can't trigger a pointless reconnect-then-fail-the-same-query loop. `poll_fast_with_reconnect()` (shared by the fast ticker and manual refresh) reconnects only when `send_fast` reports `connection_dead`.

  **Error message formatting**: `anyhow::Error`'s plain `{}`/`.to_string()` shows only its outermost wrapper — for a `tokio_postgres::Error` that's a near-useless generic label ("db error"), not the actual server ERROR/DETAIL/HINT text, which sits one level down in the `source()` chain. Every error sent as `AppEvent::Error` uses `format!("{e:#}")` (anyhow's alternate format walks the chain) for `anyhow::Error`s, or the hand-rolled `chained_message()` for a raw `tokio_postgres::Error` (`run_signal`'s `client.execute()` return, which isn't anyhow-wrapped and whose bare `Display` doesn't auto-chain either). Verified end-to-end by deliberately injecting a bad column reference — confirms the visible error text is the real Postgres message, not the generic wrapper.

  `Cancel`/`Terminate` run `SELECT pg_cancel_backend($1)`/`pg_terminate_backend($1)` on the task's own `Client` (must go through this channel — the render loop can't reach the `Client` directly) via `run_signal()`, reporting the result as `AppEvent::Status` (success) or `AppEvent::Error` (failure, e.g. lacking `pg_signal_backend` — surfaces as a status message, not a panic; confirmed against a role without that grant).
- `src/db/connections.rs` — unchanged from v1: used/max/active/idle/idle-in-txn from `pg_stat_activity`, one aggregate query. Reused directly by `ui/activity.rs`'s summary cards.
- `src/db/cache_io.rs` (v1's `cache.rs`, renamed+extended) — **4 independent fetch fns** (`fetch_overall`, `fetch_coldest`, `fetch_bgwriter`, `fetch_replication`; a fifth, `fetch_per_database`, was deleted post-v5 — see below), not one bundled `CacheIoData` fetch — matches the 4 blocks now rendered on `ui/overview.rs` (since v4; previously their own `ui/cache_io.rs` tab) 1:1, each with its own `PanelSnapshot` variant/source label/App field, so one failing doesn't blank the others (see the per-query-isolation note above; this is the same fix applied one level deeper, inside what used to be a single panel's own bundled fetch). `fetch_overall`'s `CacheOverall` adds `xact_commit`/`xact_rollback` (cumulative — `App::record_cache_overall` derives commits/s and rollback% from the poll-to-poll delta) and temp-file stats. `fetch_bgwriter(client, pg17_plus)` branches on Postgres major version: PG13-16 query `pg_stat_bgwriter` directly; **PG17+** (`pg_stat_bgwriter` dropped `checkpoints_timed`/`checkpoints_req`/`buffers_checkpoint` — hit this live as a real user-reported bug: `column "checkpoints_timed" does not exist`) query the new `pg_stat_checkpointer` view instead (`num_timed`/`num_requested`/`buffers_written`, comma-joined with the now-smaller `pg_stat_bgwriter` for `buffers_clean`/`maxwritten_clean`/`buffers_alloc`) and map both into the same `BgWriterStats` shape. `buffers_backend` moved to `pg_stat_io` on PG17+ (needs a heavier per-backend-type aggregation, not done) — it's `BgWriterStats.buffers_backend: Option<i64>`, `None` there rather than a fabricated `0`. `pg17_plus` is derived once from `ServerInfo.version_num` (`current_setting('server_version_num')`) at connect/reconnect/db-switch, threaded into `send_fast` — see `db/mod.rs`. `fetch_replication`'s `pg_current_wal_lsn()` only evaluates per matched row, so a standby with no downstream replicas of its own never hits its primary-only error. Verified end-to-end against a real local PG17.10 instance (`brew install postgresql@17`), not just PG14 — this class of version-skew bug won't reliably show up if every test targets one Postgres version.
- `src/db/indexes.rs` — `IndexRow`/`fetch()` unchanged from v1. Adds `UnindexedForeignKey`/`fetch_unindexed_foreign_keys()`: a `pg_constraint`/`pg_index` heuristic (index's leading column matches the FK's first column, and covers all FK columns) — a real heuristic, not a guarantee; differently-ordered composite indexes can false-positive.
- `src/db/tables.rs` — extends v1's query with dead-tuple % (`n_dead_tup`/`n_live_tup`), xid age (`age(relfrozenxid)`), seq-scan count (rate computed client-side in `App::record_tables` against the previous slow-tier snapshot — reads `"—"` for ~5 minutes after startup/db-switch, expected, not a bug), index-use % (`idx_scan`/`(idx_scan+seq_scan)`), and last-(auto)vacuum age. Raw `table_bytes`/`dead_tuples`/`idx_scan` counts were fetched at one point and dropped again — the derived percentages fully cover what the UI needs, nothing consumed the raw counts.
- `src/db/triggers.rs` — `TriggerRow`/`fetch()`: joins `pg_trigger`→`pg_class`/`pg_namespace`→`pg_proc`, filtered by `NOT t.tgisinternal` (excludes the FK-constraint-backing `RI_FKey_*` triggers Postgres generates automatically — not user-facing) and the usual `nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')` schema exclusion. `pg_get_triggerdef(t.oid)` and `pg_get_functiondef(p.oid)` are fetched inline as columns on this same slow-tier (5 min) query rather than via a separate on-demand fetch when the popup opens — deliberate: matches how `tables.rs`/`indexes.rs` already fetch everything upfront on this tier, and avoids new plumbing (a `PollControl` variant + one-shot `AppEvent` + a loading state inside the popup) for a problem that doesn't exist yet, since trigger counts and function bodies are typically small. Ceiling: every function body gets refetched every 5 minutes even though only one is ever shown at a time; escape hatch is an on-demand fetch mirroring `Cancel`/`Terminate`'s single-shot `PollControl` shape, if that ever becomes real.
- `src/db/databases.rs` — extends v1's `Vec<String>` into `Vec<DatabaseRow>{name, owner, size_bytes, sessions, xact_commit, xact_rollback, cache_hit_pct, is_template}`, joining `pg_database`+`pg_stat_database`+a `pg_stat_activity` session-count aggregate. Powers `ui/picker.rs`. `DatabaseRow::unswitchable_reason()` is the single predicate both picker call sites (`main.rs`'s `handle_key` and `run_startup_picker`) check before ever attempting a switch — covers `is_template` and a new `is_reserved()` (name-matched against managed-Postgres providers' internal databases: `rdsadmin`/`azure_maintenance`/`azuresu`/`cloudsqladmin` — a heuristic against provider docs, not a catalog fact, so a same-named real user db would false-positive; known ceiling). Added after hitting this live against RDS: switching to `rdsadmin` used to attempt a real connection that `pg_hba.conf` always rejects (unrelated to `datallowconn`, which RDS sets true for its own internal use of that db), surfacing a raw multi-line FATAL in the footer/error-detail before falling back to the still-good previous connection. Now caught before any connection attempt, same as templates: dimmed in `ui/picker.rs` (`"reserved · not connectable"`), rejected on `enter` with a plain status message.
- `src/db/statements.rs` — `extension_loaded()` (cheap presence check, cached by `poll_task` at connect/reconnect so the medium tier can skip the real query entirely rather than just skip acting on an empty result) + `fetch(client, pg17_plus) -> Vec<StatementRow>` (assumes loaded; `poll_task` wraps the result in `StatementsData::{NotAvailable,Available}` based on the cached bool). Column names (`total_exec_time` etc.) are PG13+; earlier versions used `total_time` et al. — not handled, no minimum PG version is otherwise documented for this app. **Per-query I/O** (v5): besides `shared_blks_hit`/`shared_blks_read`, the row carries `shared_blks_written`/`temp_blks_read`/`temp_blks_written` and `io_time_ms`, with `StatementRow::io_blocks()`/`io_bytes()` (hits deliberately excluded — a hit never touched a device; ×8192 assumes the default `block_size`, marked with a `ponytail:` comment). `io_time_ms` is version-branched the same way `cache_io::fetch_bgwriter` is — PG13-16 `blk_read_time + blk_write_time`, PG17+ `shared_blk_read_time + shared_blk_write_time` (PG17 renamed them) — but built by a `fn query(pg17_plus) -> String` with `format!` rather than two consts, since the two variants would otherwise share 20 identical lines that can drift. Requires `pg17_plus` on the *medium* tier, which is why `send_medium` gained that param. The row cut is a UNION of `(top 100 by total_exec_time)` and `(top 50 by io blocks)`: with the old single `ORDER BY total_exec_time DESC LIMIT 100`, a query doing heavy disk I/O but modest wall time was never fetched at all, so no amount of UI work could have surfaced it. `#[cfg(test)]` covers `io_blocks()` summing the four disk fields and ignoring `shared_blks_hit`.
- `src/db/activity.rs` — `ActivityData{rows: Vec<ActivityRow>}` from `pg_stat_activity`, including `pg_blocking_pids(pid)` (built into Postgres since 9.6 — no hand-rolled `pg_locks` self-join needed for the blocking tree) and server-side-truncated query text (`left(query, 220)`). **Filtered to `state IS NOT NULL`** — background maintenance processes (checkpointer, bgwriter, walwriter, autovacuum launcher) have `state = NULL` and no query/state_change timestamp, so `duration_secs` falls back to their entire process uptime and would otherwise dominate the `ORDER BY duration_secs DESC` ahead of genuinely long-running queries (hit this exact bug during manual testing — a checkpointer alive for 40s outranked a 21s-old `pg_sleep` — fixed by the filter). An autovacuum *worker* actually running VACUUM has `state = 'active'` and still shows up, matching intent.
- `src/db/serverinfo.rs` — `ServerInfo{version, uptime_secs, current_db_owner}`, one-shot fetch (see `event.rs` above).
- `src/update.rs` (v17) — background release-check + staged-binary-swap, pure logic like the `db::*` modules above but with no `&Client`/SQL involved (it talks to GitHub, not Postgres). See the v17 Architecture note for the full design; not part of any poll tier, not gated by `PanelSnapshot`.

All `db::*::fetch()` functions are pure `async fn(&Client) -> Result<T>` — independently callable/testable, no UI coupling.

**Important invariant**: nothing in this codebase writes to stdout/stderr (`println!`/`eprintln!`) once `ratatui::init()` has run — the alt screen owns the terminal at that point, and direct writes corrupt the display in ways that look like application bugs (this was hit and fixed once already in v1: the background `Connection` driver task's error used to `eprintln!`, which visually looked like a stuck error banner during disconnect testing even though `App.error` was clearing correctly). Route anything that needs surfacing through `AppEvent` instead. Re-verified clean (grepped `src/` for stray `print!`/`println!`/`eprintln!` outside the documented pre-TUI call sites — now three: the original connect-spinner path, the onboarding prompts, and (v17) `update::apply_staged_update_if_present()`'s error log, which is deliberately the very first thing `main()` does, before `ratatui::init()`) after the v2 expansion.

The `build_conninfo()`/`quote_conninfo_value()` note from v1 still applies unchanged: libpq keyword/value conninfo strings are whitespace/quote-sensitive (an unescaped password with a space or quote used to corrupt the string) — any new code building a conninfo string by hand needs the same quoting, don't `format!` a raw value into one.

See `README.md` for user-facing usage/keybindings/tab reference.

## Scope

v18 (current): Triggers' selected-trigger detail moved from an always-visible bottom pane to a
full-screen scrollable popup (`enter` or click to open, `esc`/`q` to close) — see the v18
Architecture note. v17: background auto-update — checks GitHub for a newer release once per launch (throttled
to 24h), downloads and checksum-verifies it to a side path without ever touching the live process's
own executable, and swaps it in only at the next launch via the `self-replace` crate; on by default,
`--no-update-check` opts out. Configured to use `self_update`'s `ureq`+`rustls` backend specifically
(not its default `reqwest` one) to keep the `ring` crypto-backend constraint from the Build
requirement note above intact. Also fixed, same pass: `install.sh` could hang with no output on a
stalled connection (added `curl` timeouts/retries) — see the v17 Architecture note for both. v16:
mouse support extended beyond v10's original click-to-zoom — mouse-wheel scroll on all
4 scrollable panes (the 3 detail panes plus Playground's transcript), clicking a table row to select
it (Queries/Activity/Triggers/Tables & Indexes), and clicking a tab to switch — see the v16
Architecture note. v15: Playground's transcript and live input now render inside one shared bordered
block instead of two separately-bordered boxes (no seam between output and input, matching real psql), and
gained Tab-triggered autocomplete for SQL keywords/schema/table names (bash/readline-style inline
cycling, not a dropdown); a continuation line's prompt also dropped its repeated database name (`-> `
instead of `dbname-> `, a deliberate divergence from real psql, fixing a follow-up user report where
the repeated prefix made a buffered continuation line look like a second fresh prompt) — see the v15
Architecture note. v14: Playground rewritten from a split
"SQL"/"Output" box pair (F5-to-run) into a
psql-style REPL — a scrolling transcript of every command this session, `dbname=>`/`dbname->`
prompts, Enter runs once a statement is `;`-terminated, Up/Down recalls history inline, Ctrl+C is
psql's own dual-purpose cancel-or-clear-buffer, and result tables match real psql's `|`-separated/
right-aligned-numerics format byte-for-byte — see the v14 Architecture note. v13: fixed a crash
("Formatting argument out of range") triggered by any Playground result containing a >64KB cell, and
added stateless LIMIT/OFFSET-based pagination ("scroll for more") for single bare-`SELECT` queries,
plus a panic hook that now also cleans up mouse-capture/Kitty-keyboard terminal state on any crash —
see the v13 Architecture note. v12: Playground gained query cancellation (Ctrl+C, reusing the existing Activity-tab
cancel plumbing against the Playground connection's own backend pid) and a set of readline/
nvim-insert-mode editor chords (word/line delete, word/buffer-jump) — deliberately not full vim
modal editing, which stays out of scope — see the v12 Architecture note. v11 added Playground, a
6th tab and the app's first write-capable feature — a SQL console with its own dedicated connection
(so a slow/ad-hoc query can never freeze live monitoring), a hand-rolled multi-line editor, a
confirm guard on non-`SELECT` statements, and in-memory-only history/drafts — see the v11
Architecture note. v10 added the app's first mouse interaction — clicking a detail pane (Queries/Activity/Triggers) zooms it into a full-screen popup — see the v10 Architecture note. v9 added scrollable detail panes (`PageUp`/`PageDown`) on Queries/Triggers/Activity, replacing the clipped-content ceiling those panes had since v7 — see the v9 Architecture note. v8 fixed two related "cumulative/point-sampled data misread as current state" bugs — the wait-events chart's active-only normalization and the v6 temp-spill suspect's raw-cumulative threshold, both now rate/window-based — see the v8 Architecture note. v7 added a consistent "always-visible inline detail pane, updates with the row cursor" UX for Triggers and Activity, matching Queries — see the v7 Architecture note. v6 added 6 new diagnosis suspects (xid wraparound, connection pressure, checkpoint storm, disk spill/work_mem, replication lag, lock wait chain) and an alert-only N+1 query detector — see the v6 Architecture note and `docs/postgres-incident-research.md` for the real-incident research behind each. v5 added per-query disk I/O (bytes, disk time, temp-file spill) on the Queries tab, sortable by I/O. Five tabs (Overview, Queries, Activity, Tables & Indexes, Triggers — `CacheIo` was folded into Overview in v4), the monochrome UI, tiered polling, pause/rate control, the full-screen database picker, cancel/terminate, the Triggers tab's function-source popup, and the `g`-triggered diagnosis modal (replacing v2's always-visible diagnosis strip/alerts list) — all described above. Saved connection profiles (list/add/edit/pick, SSL/mutual-TLS support, no stored password) remain from v1.

Deliberately out of scope: deleting a saved profile in place, true time-integrated wait-event profiling (current sampling is a bounded recent-window point-sample, not `pg_wait_sampling`-grade — see `App::wait_event_samples` above), query-plan-derived index suggestions (missing-index candidates are limited to two mechanically-derivable heuristics, not fabricated column-level DDL guesses — see `ui/tables_indexes.rs` above), and — for Playground — persisted history/drafts to disk, real SQL tokenization for the confirm-guard's statement split and the Enter-completeness check (see the v11/v14 Architecture notes), and full vim modal editing (normal/insert/visual modes — v12 added non-modal readline/nvim-insert-mode chords instead, see the v12 Architecture note). Query cancellation from Playground shipped in v12. Pagination for `WITH`/multi-statement/DML results and cursor-held (vs. stateless LIMIT/OFFSET) pagination are deliberately out of scope — see the v13 Architecture note. Also out of scope, both from v14: the literal Postgres command tag on a non-`SELECT` result (`CREATE TABLE`/`UPDATE 3` — `simple_query`'s driver-level API only exposes a row count, see the v14 note) and matching psql's blank-for-`NULL` default (kept as literal `NULL` text, a deliberate readability choice, not an oversight). Also out of scope, from v15: column-name-level autocomplete (no existing data source beyond the narrow FK-column list) and a DBeaver-style dropdown completion popup (Tab instead does bash/readline-style inline cycling, a direct user choice) — see the v15 Architecture note. Also out of scope, from v16: mouse-wheel scroll moving table row selection (only `j`/`k` and click-to-select do that — not a general "wheel = arrow keys" mapping) and a pixel-exact tab-bar click hit test (an equal-width approximation is used instead, since `ratatui::widgets::Tabs` exposes no per-segment layout API) — see the v16 Architecture note. See README for the full deferred list.

**Keep this file in sync as features land** — update the Architecture section whenever the codebase changes in a way future instances would need to know about.
