---
name: add-panel
description: "Use when adding a new dashboard data source, block, or tab to pgpilot (the Postgres monitoring TUI in this repo). Triggers: 'add a new tab/panel to pgpilot', 'add a new data source', 'wire up a new pg_stat_* view', 'add a new block to Overview/Queries/Activity/etc.'"
---

# Add a panel to pgpilot

Adding a new data source (a block on an existing tab, or a whole new tab) is a multi-file wiring job with an established order — this has been done 4+ times already (see CLAUDE.md's v2-v5 architecture notes) and always touches the same files in the same order. Follow this checklist instead of re-deriving it.

1. **`src/db/<name>.rs`** (or a new `fetch_*` fn in an existing module, e.g. `cache_io.rs`, if the data belongs with an existing block) — a pure `async fn fetch(&Client) -> Result<T>`, independently testable, no UI coupling. Check `db/mod.rs`'s tier lists first to decide whether this extends an existing module rather than creating a new file.

2. **`src/event.rs`** — add a new `PanelSnapshot` variant + a `source_label()` arm matching the label you'll pass to `send_labeled`.

3. **`src/app.rs`** — add the `App` field: `Option<T>` normally, or `Option<(T, Instant)>` if a rate needs to be derived from the previous value (precedent: `cache_overall`, `tables`, `databases`, `statements`). Add a `record_*()` method only if a derived rate is needed.
   - If this is a **new tab** (not a block on an existing tab): also add to `PanelKind`, its `ALL` const, and `title()`; add `_state`/`_sort` fields; wire into `active_row_count()`/`scroll_down()`/`scroll_up()`/`cycle_sort()`.

4. **`src/db/mod.rs`** — wire the fetch into the correct tier (fast/medium/slow) via `send_labeled()`. **Never** bundle it into an existing `?`-chained fetch in that tier — this is the exact bug already hit live: one broken query blocked every sibling query in its tier because they shared one fallible function. Each query must fail independently.

5. **`src/main.rs`**'s `apply_event()` — route the new `PanelSnapshot` variant to the `App` field or `record_*` method.

6. **`src/ui/<tab>.rs`** (new file if new tab) — write the draw fn using:
   - `theme::block()` for any bordered panel — never hand-roll `Block::default()...title(...)`. Documented footgun: `Block::title()` with a plain string silently inherits `border_style`'s color when no title style is set, which produces a near-invisible title (`BORDER` sits only ~2 RGB shades above `PANEL_BG`). `theme::block()` sets the title style explicitly.
   - `widgets::loading_or_error()` for the "no data yet" state — never bare `loading()`. This is what makes a broken block show as *broken* (red border, error text, `(e: full error)` hint) instead of spinning forever.

7. If new tab: wire into `ui/mod.rs::draw()`'s dispatch, `widgets::draw_tab_bar()`'s key hint, the footer help text, and `main.rs::handle_key()`'s number-key dispatch.

8. Update CLAUDE.md's Architecture section with what changed (standing rule already in that file — do it now, not at the end when it's easy to forget).

9. **Test against a real/throwaway Postgres.** If the new query touches a view/column known to differ by version, branch on `pg17_plus` (precedent: `cache_io::fetch_bgwriter`, `statements::query`) and test against both an old and new cluster — don't assume one PG version's schema is universal.
