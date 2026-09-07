mod app;
mod cli;
mod completion;
mod config;
mod db;
mod diagnosis;
mod editor;
mod event;
mod format;
mod onboarding;
mod ui;
mod update;

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use app::{App, PanelKind, PlaygroundComplete, PlaygroundEntry};
use clap::Parser;
use cli::{Cli, ConnParts};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyModifiers, KeyboardEnhancementFlags,
    MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use db::playground::{playground_task, PlaygroundControl, StatementResult};
use editor::SqlEditor;
use db::tls::TlsMode;
use db::PollControl;
use event::{AppEvent, PanelSnapshot, StatusLevel};
use futures_util::StreamExt;
use tokio::sync::{mpsc, Notify};

/// Fast-tier poll intervals cycled by `-`/`+`, matching the design mockup's
/// own list.
const RATE_OPTIONS_MS: [u64; 8] = [250, 500, 1000, 1500, 2000, 3000, 5000, 10000];

/// Lines scrolled per `PageUp`/`PageDown` press in a detail pane (see
/// `App::detail_scroll`).
const DETAIL_SCROLL_STEP: u16 = 5;

/// Lines scrolled per mouse-wheel notch — smaller than `DETAIL_SCROLL_STEP`
/// (that one's sized for a keyboard page-jump); 3 matches typical
/// terminal-app wheel behavior (less/vim).
const MOUSE_SCROLL_STEP: u16 = 3;

/// Cap on `App::playground_transcript` — in-memory/session-scoped only, but
/// still bounded so a long session running many queries doesn't grow it
/// forever.
const PLAYGROUND_HISTORY_CAP: usize = 50;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Swap in a previously-staged update, if any — must happen before
    // anything else touches the terminal or a connection, since a fresh
    // process is the only safe place to replace its own executable file.
    // Errors are non-fatal (stderr is still safe here, pre-ratatui::init()):
    // a corrupted staged update must never block startup.
    if let Err(e) = update::apply_staged_update_if_present() {
        eprintln!("warning: failed to apply staged update: {e:#}");
    }

    let cli = Cli::parse();
    let (mut conninfo, tls, conn_parts) = resolve_conninfo(&cli)?;
    let interval = Duration::from_secs(cli.interval.max(1));

    // Connect and fail fast, with a clean stderr message, before entering
    // the TUI alt-screen. The same client is then handed to the background
    // poll task rather than reconnecting a second time.
    let mut client = connect_with_spinner(&conninfo, &tls)
        .await
        .context("failed to connect to Postgres")?;

    let (tx, rx) = mpsc::channel::<AppEvent>(32);
    let (control_tx, control_rx) = mpsc::channel::<PollControl>(8);
    let refresh_now = Arc::new(Notify::new());

    let current_dbname = conn_parts.as_ref().map(|p| p.dbname.clone());
    let server_addr = conn_parts.as_ref().map(|p| format!("{}:{}", p.host, p.port));
    let can_switch_db = conn_parts.is_some();

    let mut app = App::new(current_dbname.clone(), server_addr, can_switch_db, cli.ascii, interval);
    let mut terminal = ratatui::init();
    // ratatui::init() doesn't enable mouse reporting on its own. See the
    // panic hook installed just below for why a crash no longer leaves this
    // enabled.
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    // Kitty keyboard-protocol negotiation: without this, Ctrl+Enter/Cmd+Enter are
    // indistinguishable from plain Enter on the wire (both send `\r`), so Playground's
    // Ctrl+Enter/Cmd+Enter execute shortcuts silently never fire — see CLAUDE.md's v11
    // note. `supports_keyboard_enhancement()` queries the real terminal; terminals that
    // don't support the protocol (Terminal.app, plain xterm, Windows) get `Ok(false)`
    // and nothing changes for them — F5 remains their only execute key, unchanged from
    // today. See the panic hook installed just below for why a crash no longer leaves
    // this enabled either.
    let kitty_keyboard = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
        && crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();

    // `ratatui::init()` already installs a panic hook that restores raw
    // mode/leaves the alt screen, then chains to whatever hook was
    // previously installed (the default one, which prints the panic). It
    // doesn't know about the two terminal modes just enabled above, which
    // used to leave a crashed session's shell in mouse-report/altered-
    // keyboard-protocol mode until the next `reset`/new shell — hit for
    // real by a user (see CLAUDE.md's v11 Playground note on the crash this
    // was found alongside). Layered here rather than before `init()`
    // because `kitty_keyboard` isn't known until just above; cleanup runs
    // first (while the alt-screen buffer ratatui's hook is about to clear
    // is still active, not the now-visible normal screen) then delegates.
    // Note: `set_hook` is process-global, so a panic inside a background
    // task (`playground_task`/`poll_task`) fires this too, mid-session —
    // harmless, the cleanup is idempotent, but disclosed rather than assumed.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
        if kitty_keyboard {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        prev_hook(info);
    }));

    if can_switch_db {
        // Show the database list (current db pre-selected) before starting
        // any polling — no `poll_task` is spawned yet, so nothing is fetched
        // from any database in the background until the user resolves this.
        if let Ok(rows) = db::databases::fetch(&client).await {
            app.databases = Some((rows, Instant::now()));
        }
        app.open_db_popup();

        if let Some(chosen) = run_startup_picker(&mut terminal, &mut app).await?
            && Some(chosen.as_str()) != current_dbname.as_deref()
            && let Some(base) = &conn_parts
        {
            let new_conninfo =
                cli::build_conninfo(&base.host, base.port, &base.user, &chosen, base.password.as_deref());
            match db::connect(&new_conninfo, &tls).await {
                Ok(new_client) => {
                    client = new_client;
                    conninfo = new_conninfo;
                    app.current_dbname = Some(chosen);
                }
                Err(e) => {
                    app.set_status(format!("failed to switch to '{chosen}': {e:#}"), StatusLevel::Warn);
                }
            }
        }
    }

    // A second, dedicated connection for the Playground tab (see CLAUDE.md's
    // v11 note) — `db::poll_task`'s one `Client` is fully occupied with
    // tiered polling, so a slow/lock-held ad-hoc query on that same
    // connection would freeze every live panel for its duration. Connected
    // eagerly here (same pattern as `poll_task`'s own connect-once-and-spawn
    // shape) rather than lazily on first tab entry, which would need a new
    // "spawn a task from the sync key-handler path" pattern this codebase
    // doesn't otherwise have. Must happen *before* `conninfo`/`tls`/
    // `conn_parts` are moved into `poll_task`'s spawn call below.
    let (playground_tx, playground_rx) = mpsc::channel::<PlaygroundControl>(8);
    match db::connect(&conninfo, &tls).await {
        Ok(playground_client) => {
            tokio::spawn(playground_task(playground_client, tls.clone(), conn_parts.clone(), tx.clone(), playground_rx));
        }
        Err(e) => {
            // No retry within the session (ponytail: restart to retry) —
            // shown as a permanent error block on the Playground tab.
            app.playground_conn_error = Some(format!("{e:#}"));
        }
    }

    tokio::spawn(db::poll_task(
        client,
        conninfo,
        tls,
        conn_parts,
        interval,
        tx.clone(),
        refresh_now.clone(),
        control_rx,
    ));

    // Fire-and-forget background release check — must not delay getting
    // into the Postgres session, so it's spawned last and reports back over
    // the same `tx` channel every other background task already uses.
    if !cli.no_update_check {
        tokio::spawn(update::update_check_task(tx));
    }

    let result = run(&mut terminal, &mut app, rx, refresh_now, control_tx, playground_tx).await;
    if kitty_keyboard {
        let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Drives just the startup database picker (`app.db_popup` already open
/// before this is called) — runs before `poll_task` is spawned, so no panel
/// is being fetched from any database yet. Returns the chosen dbname on
/// `enter` over a switchable row (`None` if it's the same db already
/// connected, or a template/reserved row — see `DatabaseRow::unswitchable_reason`
/// — no reconnect attempted either way), or `None` on `esc`/`q`/`d`.
async fn run_startup_picker(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
) -> anyhow::Result<Option<String>> {
    let mut reader = EventStream::new();
    let mut render_tick = tokio::time::interval(Duration::from_millis(250));

    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        tokio::select! {
            maybe_event = reader.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    match key.code {
                        KeyCode::Down | KeyCode::Char('j') => app.popup_move(1),
                        KeyCode::Up | KeyCode::Char('k') => app.popup_move(-1),
                        KeyCode::Enter => {
                            let selected = app.popup_selected_database().map(|row| (row.name.clone(), row.unswitchable_reason()));
                            app.close_db_popup();
                            return Ok(match selected {
                                Some((name, None)) => Some(name),
                                Some((_, Some(reason))) => {
                                    app.set_status(reason.to_string(), StatusLevel::Warn);
                                    None
                                }
                                None => None,
                            });
                        }
                        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('d') => {
                            app.close_db_popup();
                            return Ok(None);
                        }
                        _ => {}
                    }
                }
            }
            _ = render_tick.tick() => {
                app.spinner_frame = app.spinner_frame.wrapping_add(1);
            }
        }
    }
}

/// Connects with an animated spinner on the plain (pre-TUI) terminal, so a
/// slow/unreachable host doesn't look like a silent hang. Runs entirely
/// before `ratatui::init()` — ordinary stdout writes, not a ratatui widget.
async fn connect_with_spinner(
    conninfo: &str,
    tls: &TlsMode,
) -> anyhow::Result<tokio_postgres::Client> {
    let connect_fut = db::connect(conninfo, tls);
    tokio::pin!(connect_fut);

    let mut ticker = tokio::time::interval(Duration::from_millis(80));
    let mut frame = 0usize;

    let result = loop {
        tokio::select! {
            res = &mut connect_fut => break res,
            _ = ticker.tick() => {
                print!("\r{}  Connecting to Postgres...", ui::widgets::spinner(frame));
                let _ = std::io::stdout().flush();
                frame = frame.wrapping_add(1);
            }
        }
    };

    // Clear the spinner line regardless of outcome.
    print!("\r\x1b[K");
    let _ = std::io::stdout().flush();

    result
}

/// Resolves the connection string, TLS settings, and (when available) the
/// separate connection parts used for the 'd' switch-database popup, in
/// order:
/// 1. `--dsn`/`--host`/`--port`/`--user`/`--dbname` (flag or libpq env var)
///    given explicitly → build from those, exactly as before this feature
///    existed. No prompts, so scripted/non-interactive use is unaffected.
///    TLS comes from `--ssl*` flags here (`--dsn` itself stays TLS-disabled
///    — we don't parse sslmode out of an arbitrary user-supplied DSN string).
///    `ConnParts` is `None` for `--dsn` (see `Cli::conn_parts`), `Some` for
///    `--host`/etc.
/// 2. `--profile NAME` → look up a saved profile directly.
/// 3. Bare invocation → the saved-connections picker (or, with no saved
///    profiles yet, straight into the wizard; 'e' then a number edits a
///    profile in place).
///
/// Cases 2 and 3 then resolve the password from `PGPASSWORD`, or an
/// interactive masked prompt if that's unset — profiles never store one.
fn resolve_conninfo(cli: &Cli) -> anyhow::Result<(String, TlsMode, Option<ConnParts>)> {
    if cli.has_explicit_connection_info() {
        return Ok((cli.connection_string(), cli.tls_mode(), cli.conn_parts()));
    }

    let mut cfg = config::load().context("failed to load saved connections")?;

    let profile = if let Some(name) = &cli.profile {
        cfg.profiles.get(name).cloned().with_context(|| {
            format!("no saved connection named '{name}' (run with no --profile to see the list)")
        })?
    } else {
        match onboarding::list_and_pick(&mut cfg)? {
            onboarding::PickResult::Existing(p) | onboarding::PickResult::New(p) => p,
            onboarding::PickResult::Quit => std::process::exit(0),
        }
    };

    let password = match std::env::var("PGPASSWORD") {
        Ok(p) => Some(p),
        Err(_) => onboarding::prompt_password(),
    };

    let conninfo = cli::build_conninfo(
        &profile.host,
        profile.port,
        &profile.user,
        &profile.dbname,
        password.as_deref(),
    );
    let tls = TlsMode::from_parts(
        profile.ssl,
        profile.ssl_root_cert,
        profile.ssl_client_cert,
        profile.ssl_client_key,
    );
    let parts = ConnParts {
        host: profile.host,
        port: profile.port,
        user: profile.user,
        password,
        dbname: profile.dbname,
    };

    Ok((conninfo, tls, Some(parts)))
}

/// Merges keyboard input, DB poll snapshots, and a UI heartbeat tick into a
/// single loop via `tokio::select!`, so keystrokes stay responsive
/// regardless of DB poll timing (the poll runs in its own task).
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mut rx: mpsc::Receiver<AppEvent>,
    refresh_now: Arc<Notify>,
    control_tx: mpsc::Sender<PollControl>,
    playground_tx: mpsc::Sender<PlaygroundControl>,
) -> anyhow::Result<()> {
    let mut reader = EventStream::new();
    // Decoupled from the (slower) DB poll interval so the UI redraws
    // promptly on keystrokes and keeps "last updated" freshness visible.
    let mut render_tick = tokio::time::interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => handle_key(app, key.code, key.modifiers, &refresh_now, &control_tx, &playground_tx),
                    Some(Ok(Event::Mouse(mouse))) => handle_mouse(app, mouse, &playground_tx),
                    _ => {}
                }
            }
            Some(event) = rx.recv() => {
                apply_event(app, event, &playground_tx);
            }
            _ = render_tick.tick() => {
                app.spinner_frame = app.spinner_frame.wrapping_add(1);
            }
        }

        terminal.draw(|frame| ui::draw(frame, app))?;

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn handle_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    refresh_now: &Arc<Notify>,
    control_tx: &mpsc::Sender<PollControl>,
    playground_tx: &mpsc::Sender<PlaygroundControl>,
) {
    if app.error_detail_open {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('e')) {
            app.error_detail_open = false;
        }
        return;
    }

    if app.diagnosis_open {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('g')) {
            app.diagnosis_open = false;
        }
        return;
    }

    if app.detail_popup_open {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => app.detail_popup_open = false,
            KeyCode::PageDown => app.detail_scroll = app.detail_scroll.saturating_add(DETAIL_SCROLL_STEP),
            KeyCode::PageUp => app.detail_scroll = app.detail_scroll.saturating_sub(DETAIL_SCROLL_STEP),
            _ => {}
        }
        return;
    }

    if app.db_popup.is_some() {
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('d') => app.close_db_popup(),
            KeyCode::Down | KeyCode::Char('j') => app.popup_move(1),
            KeyCode::Up | KeyCode::Char('k') => app.popup_move(-1),
            KeyCode::Enter => {
                if let Some(row) = app.popup_selected_database() {
                    if let Some(reason) = row.unswitchable_reason() {
                        app.set_status(reason.to_string(), StatusLevel::Warn);
                    } else if Some(row.name.as_str()) != app.current_dbname.as_deref() {
                        let _ = control_tx.try_send(PollControl::SwitchDb(row.name.clone()));
                    }
                }
                app.close_db_popup();
            }
            _ => {}
        }
        return;
    }

    // The Playground tab captures every plain key for its editor — typing
    // SQL must not trigger the tab-switch/sort/quit/etc. shortcuts below.
    // Safe to check after the four overlay branches above: reaching here
    // with `app.active == Playground` means none of them are open (they
    // each swallow the digit keys needed to get to Playground while open),
    // and Playground itself swallows the keys that would open any of them.
    if app.active == PanelKind::Playground {
        handle_playground_key(app, code, modifiers, control_tx, playground_tx);
        return;
    }

    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('1') => app.set_active(PanelKind::Overview),
        KeyCode::Char('2') => app.set_active(PanelKind::Queries),
        KeyCode::Char('3') => app.set_active(PanelKind::Activity),
        KeyCode::Char('4') => app.set_active(PanelKind::TablesIndexes),
        KeyCode::Char('5') => app.set_active(PanelKind::Triggers),
        KeyCode::Char('6') => app.set_active(PanelKind::Playground),
        KeyCode::Char('d') => app.open_db_popup(),
        KeyCode::Char('e') if !app.errors.is_empty() => app.error_detail_open = true,
        KeyCode::Char('g') => app.diagnosis_open = true,
        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
        // Scrolls whichever tab's detail pane is visible (Queries/Activity/
        // Triggers) — a no-op on tabs without one, since only those three
        // draw functions read `detail_scroll`.
        KeyCode::PageDown => app.detail_scroll = app.detail_scroll.saturating_add(DETAIL_SCROLL_STEP),
        KeyCode::PageUp => app.detail_scroll = app.detail_scroll.saturating_sub(DETAIL_SCROLL_STEP),
        KeyCode::Char('s') => app.cycle_sort(),
        KeyCode::Char('r') => {
            refresh_now.notify_one();
            app.set_status("refreshed", StatusLevel::Info);
        }
        KeyCode::Char(' ') => {
            app.paused = !app.paused;
            app.set_status(
                if app.paused { "paused" } else { "resumed \u{b7} live" },
                if app.paused { StatusLevel::Warn } else { StatusLevel::Info },
            );
            let _ = control_tx.try_send(PollControl::TogglePause);
        }
        // '+' moves toward faster (lower ms) intervals, '-' toward slower.
        KeyCode::Char('+') | KeyCode::Char('=') => bump_rate(app, control_tx, -1),
        KeyCode::Char('-') | KeyCode::Char('_') => bump_rate(app, control_tx, 1),
        KeyCode::Char('x') => cancel_or_terminate(app, control_tx, PollControl::Cancel as fn(i32) -> PollControl),
        KeyCode::Char('X') => cancel_or_terminate(app, control_tx, PollControl::Terminate as fn(i32) -> PollControl),
        _ => {}
    }
}

/// Key handling while the Playground tab is active — the editor owns every
/// plain key; only these carved-out combos do something else (see
/// CLAUDE.md's v14 note, the psql-style REPL rewrite): plain **Enter** runs
/// the buffer once it's a `;`-terminated statement (psql's own rule),
/// otherwise inserts a newline for a continuation line; **F5**/Ctrl+Enter/
/// Cmd+Enter force-run whatever's in the buffer right now regardless of a
/// trailing `;` (an escape hatch for the rare case the naive completeness
/// check under/over-shoots); **Up/Down** at the top/bottom row of the
/// buffer recall previous/next history entries (readline's own multi-line
/// behavior — mid-buffer, they still move the cursor); **Ctrl+C** cancels a
/// running query; **Ctrl+Q** quits (plain `q` must stay typeable in SQL);
/// **Esc** leaves the tab — or first backs out of a pending confirmation;
/// **PageUp/PageDown** scroll the transcript (and PageDown past the bottom
/// of a paginated result fetches the next page); **Tab** completes SQL
/// keywords/schema/table names (`playground_autocomplete`), cycling through
/// matches on consecutive presses; and a handful of readline/nvim-insert-
/// mode-style chords (Ctrl+W/U/K, Ctrl+Left/Right, Ctrl+Home/End) do word/
/// line editing the plain arrow/backspace keys don't cover.
fn handle_playground_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    control_tx: &mpsc::Sender<PollControl>,
    playground_tx: &mpsc::Sender<PlaygroundControl>,
) {
    if app.playground_conn_error.is_some() {
        // Nothing else can do anything useful without a connection.
        if code == KeyCode::Esc {
            app.set_active(PanelKind::Overview);
        }
        return;
    }

    // Any key except manual scrolling snaps the transcript back to the live
    // prompt — matches how a real terminal returns to the bottom once you
    // resume typing after scrolling back through output. `draw_scrollable`
    // clamps an over-large value down to the real bottom on its own (see
    // its own doc), so the sentinel just needs to be "large enough".
    if !matches!(code, KeyCode::PageUp | KeyCode::PageDown) {
        app.detail_scroll = u16::MAX;
    }

    // A completion cycle only continues across *consecutive* Tab presses —
    // any other key (an edit, a cursor move, running the query) invalidates
    // it, so the next Tab press starts fresh from the word under the cursor.
    if !matches!(code, KeyCode::Tab) {
        app.playground_complete = None;
    }

    match code {
        KeyCode::F(5) => playground_execute(app, playground_tx),
        KeyCode::Tab => playground_autocomplete(app),
        KeyCode::Enter if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
            playground_execute(app, playground_tx)
        }
        KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        // Ctrl+C: cancel a running query — psql's own binding for this, see
        // `playground_cancel`.
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => playground_cancel(app, control_tx),
        // readline/nvim-insert-mode word editing — a buffer-mutating key, so
        // it clears a pending confirmation same as Backspace/typing below.
        KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.playground_editor.delete_word_backward();
            app.playground_confirm_pending = false;
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.playground_editor.delete_to_line_start();
            app.playground_confirm_pending = false;
        }
        KeyCode::Char('k') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.playground_editor.delete_to_line_end();
            app.playground_confirm_pending = false;
        }
        KeyCode::Esc => {
            if app.playground_confirm_pending {
                app.playground_confirm_pending = false;
            } else {
                app.set_active(PanelKind::Overview);
            }
        }
        // Pressing PageDown while already at (or past) the bottom of the
        // transcript, with more rows known available on the last entry and
        // no fetch already in flight, loads the next page — the TUI analog
        // of scroll-triggered infinite scroll, reusing the same key already
        // used to scroll rather than a new binding. Shared with mouse-wheel
        // scroll-down, see `playground_scroll_down`.
        KeyCode::PageDown => playground_scroll_down(app, playground_tx),
        // Normalizes the "pinned to bottom" sentinel via the last frame's
        // known max before subtracting — otherwise `u16::MAX - step` is
        // still huge and gets re-clamped straight back to the bottom next
        // frame, and PageUp would never actually move. Shared with
        // mouse-wheel scroll-up, see `playground_scroll_up`.
        KeyCode::PageUp => playground_scroll_up(app),
        // Any buffer-mutating key invalidates a pending confirmation — a
        // stale confirm must not silently survive an edit to the text it was
        // confirming (see CLAUDE.md's v11 confirm-guard state machine note).
        KeyCode::Enter => playground_enter(app, playground_tx),
        KeyCode::Backspace => {
            app.playground_editor.backspace();
            app.playground_confirm_pending = false;
        }
        // Word/buffer navigation (nvim's `w`/`b`/`gg`/`G`, as chords since
        // the editor is non-modal and those letters must stay typable).
        KeyCode::Left if modifiers.contains(KeyModifiers::CONTROL) => app.playground_editor.move_word_left(),
        KeyCode::Right if modifiers.contains(KeyModifiers::CONTROL) => app.playground_editor.move_word_right(),
        KeyCode::Home if modifiers.contains(KeyModifiers::CONTROL) => app.playground_editor.move_to_buffer_start(),
        KeyCode::End if modifiers.contains(KeyModifiers::CONTROL) => app.playground_editor.move_to_buffer_end(),
        // At the buffer's top/bottom row, Up/Down recall history (psql's own
        // readline behavior for a multi-line input) — mid-buffer they still
        // just move the cursor, so composing a multi-line query stays usable.
        KeyCode::Up if app.playground_editor.cursor_row() == 0 => playground_history_prev(app),
        KeyCode::Down if app.playground_editor.cursor_row() + 1 == app.playground_editor.lines().len() => {
            playground_history_next(app)
        }
        KeyCode::Left => app.playground_editor.move_left(),
        KeyCode::Right => app.playground_editor.move_right(),
        KeyCode::Up => app.playground_editor.move_up(),
        KeyCode::Down => app.playground_editor.move_down(),
        KeyCode::Home => app.playground_editor.home(),
        KeyCode::End => app.playground_editor.end(),
        // Only a plain (or shifted) character actually types text — an
        // unbound Ctrl+<letter> combo (anything not matched above) must not
        // fall through to inserting that letter literally.
        KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
            app.playground_editor.insert_char(c);
            app.playground_confirm_pending = false;
        }
        _ => {}
    }
}

/// Scroll-down-in-Playground logic shared by the `PageDown` key and
/// mouse-wheel scroll-down (`handle_mouse_scroll`): advances `detail_scroll`,
/// and triggers `PlaygroundControl::FetchMore` once scrolled at/past the
/// last frame's known bottom, if more rows are available and nothing's
/// already in flight.
fn playground_scroll_down(app: &mut App, playground_tx: &mpsc::Sender<PlaygroundControl>) {
    app.detail_scroll = app.detail_scroll.saturating_add(DETAIL_SCROLL_STEP);
    if app.detail_scroll >= app.playground_max_scroll
        && let Some(entry) = app.playground_transcript.back_mut()
        && entry.has_more
        && !entry.fetching_more
    {
        entry.fetching_more = true;
        let _ = playground_tx.try_send(PlaygroundControl::FetchMore);
    }
}

/// Scroll-up-in-Playground logic shared by the `PageUp` key and mouse-wheel
/// scroll-up: normalizes the "pinned to bottom" `u16::MAX` sentinel via the
/// last frame's known max before subtracting — otherwise `u16::MAX - step`
/// is still huge and gets re-clamped straight back to the bottom next frame,
/// and scrolling up would never actually move.
fn playground_scroll_up(app: &mut App) {
    app.detail_scroll = app.detail_scroll.min(app.playground_max_scroll).saturating_sub(DETAIL_SCROLL_STEP)
}

/// Ctrl+C, psql's own dual-purpose binding: while a query is running, cancel
/// it; otherwise, discard whatever's in the input buffer (including a
/// multi-line continuation) and start fresh — psql uses this as its "I typed
/// myself into a corner, give me a clean prompt" escape hatch, distinct from
/// Esc (which leaves the tab entirely).
///
/// The cancel path reuses the *existing* `pg_cancel_backend` plumbing
/// (`PollControl::Cancel`, already used by the Activity tab's `x` key) —
/// sent over `poll_task`'s own connection, which is idle regardless of how
/// long the Playground query runs, targeting the Playground connection's own
/// backend pid (`App::playground_pid`, captured at connect/reconnect via
/// `AppEvent::PlaygroundPid`). No new cancellation mechanism, and unlike `x`
/// targeting another session's backend, this never needs the
/// `pg_signal_backend` role — both connections are opened with the same
/// credentials, and a role can always cancel its own backend.
fn playground_cancel(app: &mut App, control_tx: &mpsc::Sender<PollControl>) {
    if app.playground_running {
        if let Some(pid) = app.playground_pid {
            let _ = control_tx.try_send(PollControl::Cancel(pid));
        }
    } else {
        app.playground_editor = SqlEditor::default();
        app.playground_confirm_pending = false;
        app.playground_history_cursor = None;
    }
}

/// Plain Enter: psql's own rule — runs the buffer once it's a complete,
/// `;`-terminated statement; otherwise inserts a newline and waits for more
/// (a continuation line). A pending confirmation always takes this Enter as
/// the confirm, regardless of what the (unchanged, since edits clear the
/// pending flag) buffer currently ends with — matches F5's existing
/// "press again to confirm" contract. ponytail: `ends_with(';')` doesn't
/// understand a `;` inside a string/dollar-quoted literal — same disclosed
/// heuristic `db::playground::split_statements` already carries; F5/
/// Ctrl+Enter stay available to force a run past it.
fn playground_enter(app: &mut App, playground_tx: &mpsc::Sender<PlaygroundControl>) {
    if app.playground_editor.is_empty() {
        return;
    }
    if app.playground_confirm_pending || app.playground_editor.text().trim_end().ends_with(';') {
        playground_execute(app, playground_tx);
    } else {
        app.playground_editor.newline();
    }
}

/// Submits the editor's current text: a no-op if already running or blank;
/// if it contains a non-`SELECT` statement and hasn't been confirmed yet,
/// sets the pending flag and returns without running (the same key press
/// again confirms — see `handle_playground_key`/`playground_enter`);
/// otherwise pushes a new (still-running) transcript entry, clears the
/// draft, and sends it off.
fn playground_execute(app: &mut App, playground_tx: &mpsc::Sender<PlaygroundControl>) {
    if app.playground_running {
        return;
    }
    let text = app.playground_editor.text();
    if text.trim().is_empty() {
        return;
    }
    if !app.playground_confirm_pending && db::playground::needs_confirmation(&text) {
        app.playground_confirm_pending = true;
        return;
    }
    app.playground_confirm_pending = false;
    app.playground_running = true;
    app.playground_transcript.push_back(PlaygroundEntry {
        sql: text.clone(),
        result: None,
        has_more: false,
        fetching_more: false,
    });
    if app.playground_transcript.len() > PLAYGROUND_HISTORY_CAP {
        app.playground_transcript.pop_front();
    }
    app.playground_editor = SqlEditor::default();
    app.playground_history_cursor = None;
    let _ = playground_tx.try_send(PlaygroundControl::Run(text));
}

/// Up/Down at the top/bottom row of the current input recall previous/next
/// commands from the transcript into the editor — psql/readline's own
/// history-recall behavior. ponytail: unlike full readline, editing a
/// recalled entry and then pressing Up/Down again discards those in-place
/// edits rather than preserving an "unsaved" slot — simpler, rarely noticed.
fn playground_history_prev(app: &mut App) {
    if app.playground_transcript.is_empty() {
        return;
    }
    let prev = match app.playground_history_cursor {
        None => app.playground_transcript.len() - 1,
        Some(0) => 0,
        Some(i) => i - 1,
    };
    app.playground_history_cursor = Some(prev);
    app.playground_editor.set_text(&app.playground_transcript[prev].sql);
    app.playground_confirm_pending = false;
}

fn playground_history_next(app: &mut App) {
    match app.playground_history_cursor {
        None => {}
        Some(i) if i + 1 < app.playground_transcript.len() => {
            app.playground_history_cursor = Some(i + 1);
            app.playground_editor.set_text(&app.playground_transcript[i + 1].sql);
        }
        Some(_) => {
            app.playground_history_cursor = None;
            app.playground_editor = SqlEditor::default();
        }
    }
    app.playground_confirm_pending = false;
}

/// Tab-completion for the Playground editor: SQL keywords, schema names,
/// and table names, sourced from `app.tables` — the slow-tier snapshot
/// populated independently by the *main* polling connection (see
/// CLAUDE.md's v11 note on why Playground has its own separate connection),
/// so this needs no new SQL/fetch of its own. A first Tab press completes
/// the word before the cursor with the first match; consecutive presses
/// (tracked via `app.playground_complete`, reset on any other key in
/// `handle_playground_key`) cycle through the rest.
fn playground_autocomplete(app: &mut App) {
    let row = app.playground_editor.cursor_row();
    let cursor_col = app.playground_editor.cursor_col();
    let line = app.playground_editor.lines()[row].clone();
    let (start_col, prefix) = completion::word_before_cursor(&line, cursor_col);

    if let Some(state) = &mut app.playground_complete
        && state.row == row
        && state.start_col == start_col
        && !state.candidates.is_empty()
    {
        state.index = (state.index + 1) % state.candidates.len();
        let next = state.candidates[state.index].clone();
        app.playground_editor.replace_current_word(start_col, &next);
        return;
    }

    if prefix.is_empty() {
        app.playground_complete = None;
        return;
    }

    let tables = app.tables.as_ref().map(|(t, _)| t);
    let candidates = completion::candidates(&prefix, tables);
    let Some(first) = candidates.first().cloned() else {
        app.playground_complete = None;
        return;
    };
    app.playground_editor.replace_current_word(start_col, &first);
    app.playground_complete = Some(PlaygroundComplete { row, start_col, candidates, index: 0 });
}

/// Routes mouse input: `error_detail_open`/`diagnosis_open`/`db_popup` block
/// everything below (unrelated full-screen overlays with no scrollable
/// content), but `detail_popup_open` blocks only the click branch — its
/// content *is* what the wheel should still scroll while zoomed, same as
/// `PageUp`/`PageDown` already do in that state (see `handle_key`).
fn handle_mouse(app: &mut App, mouse: MouseEvent, playground_tx: &mpsc::Sender<PlaygroundControl>) {
    if app.error_detail_open || app.diagnosis_open || app.db_popup.is_some() {
        return;
    }
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => handle_mouse_scroll(app, mouse, playground_tx),
        MouseEventKind::Down(MouseButton::Left) if !app.detail_popup_open => handle_mouse_click(app, mouse),
        _ => {}
    }
}

/// Mouse-wheel scrolling for the 4 scrollable panes (Queries/Activity/
/// Triggers' detail pane, Playground's transcript) — the same content
/// `PageUp`/`PageDown` already scroll, keyed off the same `detail_scroll`.
/// Deliberately excluded: the wheel never moves table row selection (only
/// `j`/`k` and click-to-select, `handle_mouse_click`, do that).
fn handle_mouse_scroll(app: &mut App, mouse: MouseEvent, playground_tx: &mpsc::Sender<PlaygroundControl>) {
    let up = mouse.kind == MouseEventKind::ScrollUp;

    if app.active == PanelKind::Playground {
        // ponytail: no stored Rect for the transcript (unlike the 3 detail
        // panes below) — scroll applies unconditionally whenever this tab is
        // active rather than adding a Rect just for this. Upgrade path: give
        // playground.rs its own transcript Rect if the fixed input line
        // below ever needs independently-gated scroll.
        if up {
            playground_scroll_up(app);
        } else {
            playground_scroll_down(app, playground_tx);
        }
        return;
    }

    if !matches!(app.active, PanelKind::Queries | PanelKind::Activity | PanelKind::Triggers) {
        return;
    }
    let click = ratatui::layout::Position { x: mouse.column, y: mouse.row };
    // Zoomed: the popup fills the screen, nothing to hit-test. Unzoomed:
    // only scroll when the cursor is actually over the (small) detail pane.
    let over_pane = app.detail_popup_open || app.detail_pane_rect.is_some_and(|r| r.contains(click));
    if !over_pane {
        return;
    }
    if up {
        app.detail_scroll = app.detail_scroll.saturating_sub(MOUSE_SCROLL_STEP);
    } else {
        app.detail_scroll = app.detail_scroll.saturating_add(MOUSE_SCROLL_STEP);
    }
}

/// Left-click only (never reached while zoomed — see `handle_mouse`): tab
/// bar first, then the active tab's row table (click-to-select), then its
/// detail pane (click-to-zoom, the original v10 behavior) — checked in that
/// order since the three Rects never overlap (tab bar is `chunks[1]`,
/// table/detail panes are both within `chunks[2]`), so a click matches at
/// most one.
fn handle_mouse_click(app: &mut App, mouse: MouseEvent) {
    let click = ratatui::layout::Position { x: mouse.column, y: mouse.row };

    if let Some(rect) = app.tab_bar_rect
        && rect.contains(click)
        && let Some(idx) = tab_index_at(rect, mouse.column, mouse.row)
    {
        app.set_active(PanelKind::ALL[idx]);
        return;
    }

    let has_table = matches!(
        app.active,
        PanelKind::Queries | PanelKind::Activity | PanelKind::TablesIndexes | PanelKind::Triggers
    );
    if has_table
        && let Some(rect) = app.table_pane_rect
        && rect.contains(click)
    {
        if let Some(idx) = row_index_at(app, rect, mouse.row) {
            app.select_row_at(idx);
        }
        return;
    }

    let has_detail_pane = matches!(app.active, PanelKind::Queries | PanelKind::Activity | PanelKind::Triggers);
    if has_detail_pane && app.detail_pane_rect.is_some_and(|r| r.contains(click)) {
        app.detail_popup_open = true;
    }
}

/// Row index under `mouse_row` inside a table `Rect` — accounts for its
/// 1-row top border + 1-row header (identical for all 4 row-selectable
/// tabs' `Table`s: `theme::block()`'s `Borders::ALL` + `.header()`, row
/// height 1, no margins) and the table's current scroll offset. `None` for
/// a click on the border/header rows, past the visible rows, or when the
/// active tab has no table.
fn row_index_at(app: &App, rect: ratatui::layout::Rect, mouse_row: u16) -> Option<usize> {
    const HEADER_ROWS: u16 = 2; // 1 top border + 1 header row
    let visible_height = rect.height.saturating_sub(HEADER_ROWS + 1); // + 1 bottom border
    let relative = mouse_row.checked_sub(rect.y + HEADER_ROWS)?;
    if relative >= visible_height {
        return None;
    }
    let offset = match app.active {
        PanelKind::Queries => app.queries_state.offset(),
        PanelKind::Activity => app.activity_state.offset(),
        PanelKind::TablesIndexes => app.tables_state.offset(),
        PanelKind::Triggers => app.triggers_state.offset(),
        _ => return None,
    };
    Some(offset + relative as usize)
}

/// ponytail: `Tabs` has no public API for per-segment rendered bounds (its
/// layout is a private fn) — equal division is a known ceiling since real
/// titles vary in width ("Tables & Indexes" vs "Queries"), so a click near a
/// boundary can land one tab off. Upgrade path: sum the exact
/// title+divider+padding widths if that ever matters enough to bother.
fn tab_index_at(rect: ratatui::layout::Rect, column: u16, row: u16) -> Option<usize> {
    if row != rect.y + 1 {
        return None; // top/bottom border row, not the tab content row
    }
    let n = PanelKind::ALL.len() as u16;
    let seg_width = rect.width.saturating_sub(2).max(1) / n; // -2 for left+right border
    if seg_width == 0 {
        return None;
    }
    let idx = (column.saturating_sub(rect.x + 1) / seg_width) as usize;
    (idx < PanelKind::ALL.len()).then_some(idx)
}

fn cancel_or_terminate(app: &App, control_tx: &mpsc::Sender<PollControl>, make: fn(i32) -> PollControl) {
    if app.active == PanelKind::Activity
        && let Some(pid) = app.selected_activity_pid()
    {
        let _ = control_tx.try_send(make(pid));
    }
}

fn bump_rate(app: &mut App, control_tx: &mpsc::Sender<PollControl>, dir: i32) {
    let cur_ms = app.rate.as_millis() as i64;
    let mut idx = 0usize;
    let mut best_diff = i64::MAX;
    for (i, &ms) in RATE_OPTIONS_MS.iter().enumerate() {
        let diff = (ms as i64 - cur_ms).abs();
        if diff < best_diff {
            best_diff = diff;
            idx = i;
        }
    }
    let new_idx = (idx as i32 + dir).clamp(0, RATE_OPTIONS_MS.len() as i32 - 1) as usize;
    app.rate = Duration::from_millis(RATE_OPTIONS_MS[new_idx]);
    let _ = control_tx.try_send(PollControl::SetInterval(app.rate));
    app.set_status(format!("refresh {}", format::human_rate(app.rate)), StatusLevel::Info);
}

fn apply_event(app: &mut App, event: AppEvent, playground_tx: &mpsc::Sender<PlaygroundControl>) {
    match event {
        AppEvent::Snapshot(snapshot) => {
            // Clears only this panel's own sticky error, not every source's —
            // a poll cycle can have some panels fail and others succeed.
            app.errors.remove(snapshot.source_label());
            app.last_refresh = Some(Instant::now());
            let now = Instant::now();
            match snapshot {
                PanelSnapshot::Connections(data) => app.record_connections(data),
                PanelSnapshot::CacheOverall(data) => app.record_cache_overall(data, now),
                PanelSnapshot::CacheColdest(data) => app.cache_coldest = Some(data),
                PanelSnapshot::CacheCheckpoints(data) => app.cache_checkpoints = Some(data),
                PanelSnapshot::CacheReplication(data) => app.cache_replication = Some(data),
                PanelSnapshot::Indexes(data) => app.indexes = Some(data),
                PanelSnapshot::UnindexedForeignKeys(data) => app.unindexed_fks = Some(data),
                PanelSnapshot::Tables(data) => app.record_tables(data, now),
                PanelSnapshot::Databases(data) => app.record_databases(data, now),
                PanelSnapshot::Statements(data) => app.record_statements(data, now),
                PanelSnapshot::Activity(data) => app.record_activity(data),
                PanelSnapshot::Triggers(data) => app.triggers = Some(data),
            }
        }
        AppEvent::Error { source, message } => {
            app.errors.insert(source, message);
        }
        AppEvent::DbSwitched(name) => {
            app.on_db_switched(name.clone());
            // The Playground connection must follow, or a query typed while
            // looking at db "foo" could silently execute against a stale
            // "bar" connection (see CLAUDE.md's v11 note).
            let _ = playground_tx.try_send(PlaygroundControl::SwitchDb(name));
        }
        AppEvent::ServerInfo(info) => app.server_info = Some(info),
        AppEvent::Status(text, level) => app.set_status(text, level),
        AppEvent::PlaygroundResult { result, has_more } => {
            app.playground_running = false;
            // Only fill an entry that's still pending — a stale result from
            // a query abandoned mid-flight by a db switch must not clobber
            // whatever's since been pushed (see `App::on_db_switched`).
            if let Some(entry) = app.playground_transcript.back_mut()
                && entry.result.is_none()
            {
                entry.has_more = has_more;
                entry.result = Some(result);
            }
        }
        AppEvent::PlaygroundMore { result, has_more } => {
            // Degrades to a safe no-op if the shape doesn't match — e.g. a
            // straggling response racing a fresh `Run`/db-switch that's
            // since pushed a different last entry (disclosed: worst case one
            // stale frame before that entry's own result arrives).
            if let Some(entry) = app.playground_transcript.back_mut() {
                entry.fetching_more = false;
                entry.has_more = has_more;
                match result {
                    Ok(fetched) => {
                        if let Some(Ok(results)) = &mut entry.result
                            && let Some(StatementResult::Rows { rows, .. }) = results.last_mut()
                            && let StatementResult::Rows { rows: new_rows, .. } = fetched
                        {
                            rows.extend(new_rows);
                        }
                    }
                    Err(message) => app.set_status(format!("fetch more failed: {message}"), StatusLevel::Warn),
                }
            }
        }
        AppEvent::PlaygroundPid(pid) => app.playground_pid = Some(pid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn test_app() -> App {
        App::new(Some("db".to_string()), None, true, false, Duration::from_secs(2))
    }

    fn left_click(column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::empty() }
    }

    fn scroll_event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::empty() }
    }

    fn test_playground_tx() -> (mpsc::Sender<PlaygroundControl>, mpsc::Receiver<PlaygroundControl>) {
        mpsc::channel(8)
    }

    #[test]
    fn click_inside_detail_pane_opens_popup() {
        let mut app = test_app();
        app.active = PanelKind::Queries;
        app.detail_pane_rect = Some(Rect { x: 0, y: 10, width: 40, height: 8 });
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, left_click(5, 12), &tx);

        assert!(app.detail_popup_open);
    }

    #[test]
    fn click_outside_detail_pane_does_nothing() {
        let mut app = test_app();
        app.active = PanelKind::Queries;
        app.detail_pane_rect = Some(Rect { x: 0, y: 10, width: 40, height: 8 });
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, left_click(5, 2), &tx);

        assert!(!app.detail_popup_open);
    }

    /// A tab without a detail pane (e.g. Overview) never sets
    /// `detail_pane_rect`, but a stale `Some` left over from a previous tab
    /// must not let a click on this tab spuriously open a popup.
    #[test]
    fn click_ignored_on_tab_without_detail_pane_even_with_stale_rect() {
        let mut app = test_app();
        app.active = PanelKind::Overview;
        app.detail_pane_rect = Some(Rect { x: 0, y: 10, width: 40, height: 8 });
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, left_click(5, 12), &tx);

        assert!(!app.detail_popup_open);
    }

    #[test]
    fn click_ignored_while_another_overlay_is_open() {
        let mut app = test_app();
        app.active = PanelKind::Queries;
        app.detail_pane_rect = Some(Rect { x: 0, y: 10, width: 40, height: 8 });
        app.error_detail_open = true;
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, left_click(5, 12), &tx);

        assert!(!app.detail_popup_open);
    }

    #[test]
    fn click_ignored_while_popup_zoomed() {
        let mut app = test_app();
        app.active = PanelKind::Queries;
        app.detail_pane_rect = Some(Rect { x: 0, y: 10, width: 40, height: 8 });
        app.detail_popup_open = true;
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, left_click(5, 12), &tx);

        // Still zoomed, no row got selected — click stays a no-op while
        // already zoomed, same as before this feature.
        assert!(app.detail_popup_open);
        assert_eq!(app.queries_state.selected(), None);
    }

    #[test]
    fn scroll_inside_detail_pane_changes_detail_scroll() {
        let mut app = test_app();
        app.active = PanelKind::Queries;
        app.detail_pane_rect = Some(Rect { x: 0, y: 10, width: 40, height: 8 });
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, scroll_event(MouseEventKind::ScrollDown, 5, 12), &tx);

        assert_eq!(app.detail_scroll, MOUSE_SCROLL_STEP);
    }

    #[test]
    fn scroll_outside_detail_pane_does_nothing() {
        let mut app = test_app();
        app.active = PanelKind::Queries;
        app.detail_pane_rect = Some(Rect { x: 0, y: 10, width: 40, height: 8 });
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, scroll_event(MouseEventKind::ScrollDown, 5, 2), &tx);

        assert_eq!(app.detail_scroll, 0);
    }

    #[test]
    fn scroll_works_while_popup_zoomed_regardless_of_cursor() {
        let mut app = test_app();
        app.active = PanelKind::Queries;
        app.detail_pane_rect = None;
        app.detail_popup_open = true;
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, scroll_event(MouseEventKind::ScrollDown, 99, 99), &tx);

        assert_eq!(app.detail_scroll, MOUSE_SCROLL_STEP);
    }

    #[test]
    fn scroll_ignored_while_error_overlay_open() {
        let mut app = test_app();
        app.active = PanelKind::Queries;
        app.detail_pane_rect = Some(Rect { x: 0, y: 10, width: 40, height: 8 });
        app.error_detail_open = true;
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, scroll_event(MouseEventKind::ScrollDown, 5, 12), &tx);

        assert_eq!(app.detail_scroll, 0);
    }

    #[test]
    fn scroll_on_playground_ignores_cursor_position() {
        let mut app = test_app();
        app.active = PanelKind::Playground;
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, scroll_event(MouseEventKind::ScrollDown, 99, 99), &tx);

        assert_eq!(app.detail_scroll, DETAIL_SCROLL_STEP);
    }

    #[test]
    fn scroll_down_at_playground_bottom_triggers_fetch_more() {
        let mut app = test_app();
        app.active = PanelKind::Playground;
        app.playground_max_scroll = 10;
        app.detail_scroll = 10;
        app.playground_transcript.push_back(PlaygroundEntry {
            sql: "select 1".to_string(),
            result: Some(Ok(vec![])),
            has_more: true,
            fetching_more: false,
        });
        let (tx, mut rx) = test_playground_tx();

        handle_mouse(&mut app, scroll_event(MouseEventKind::ScrollDown, 0, 0), &tx);

        assert!(app.playground_transcript.back().unwrap().fetching_more);
        assert!(matches!(rx.try_recv(), Ok(PlaygroundControl::FetchMore)));
    }

    #[test]
    fn scroll_up_on_playground_normalizes_pinned_sentinel() {
        let mut app = test_app();
        app.active = PanelKind::Playground;
        app.detail_scroll = u16::MAX;
        app.playground_max_scroll = 20;
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, scroll_event(MouseEventKind::ScrollUp, 0, 0), &tx);

        assert_eq!(app.detail_scroll, 20 - DETAIL_SCROLL_STEP);
    }

    fn sample_trigger(name: &str) -> crate::db::triggers::TriggerRow {
        crate::db::triggers::TriggerRow {
            schema_name: "public".to_string(),
            table_name: "t".to_string(),
            trigger_name: name.to_string(),
            function_name: "f".to_string(),
            enabled: true,
            trigger_def: "CREATE TRIGGER ...".to_string(),
            function_def: "CREATE FUNCTION ...".to_string(),
        }
    }

    #[test]
    fn click_row_selects_it() {
        let mut app = test_app();
        app.active = PanelKind::Triggers;
        app.triggers = Some(vec![sample_trigger("a"), sample_trigger("b"), sample_trigger("c")]);
        app.table_pane_rect = Some(Rect { x: 0, y: 0, width: 40, height: 10 });
        let (tx, _rx) = test_playground_tx();

        // row 0 = top border, row 1 = header, row 2 = first data row (index
        // 0), row 3 = second data row (index 1)
        handle_mouse(&mut app, left_click(5, 3), &tx);

        assert_eq!(app.triggers_state.selected(), Some(1));
    }

    #[test]
    fn click_row_out_of_range_is_noop() {
        let mut app = test_app();
        app.active = PanelKind::Triggers;
        app.triggers = Some(vec![sample_trigger("a"), sample_trigger("b")]);
        app.table_pane_rect = Some(Rect { x: 0, y: 0, width: 40, height: 10 });
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, left_click(5, 7), &tx);

        assert_eq!(app.triggers_state.selected(), None);
    }

    #[test]
    fn click_table_header_or_border_row_is_noop() {
        let mut app = test_app();
        app.active = PanelKind::Triggers;
        app.triggers = Some(vec![sample_trigger("a"), sample_trigger("b")]);
        app.table_pane_rect = Some(Rect { x: 0, y: 0, width: 40, height: 10 });
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, left_click(5, 0), &tx);
        assert_eq!(app.triggers_state.selected(), None);

        handle_mouse(&mut app, left_click(5, 1), &tx);
        assert_eq!(app.triggers_state.selected(), None);
    }

    #[test]
    fn click_tab_bar_switches_tab() {
        let mut app = test_app();
        app.tab_bar_rect = Some(Rect { x: 0, y: 1, width: 60, height: 3 });
        let (tx, _rx) = test_playground_tx();

        // 6 equal segments across width 58 (60 - 2 border cols) -> ~9 wide
        // each; column inside the 3rd segment (Activity, index 2).
        handle_mouse(&mut app, left_click(20, 2), &tx);

        assert_eq!(app.active, PanelKind::Activity);
    }

    #[test]
    fn click_tab_bar_border_row_is_noop() {
        let mut app = test_app();
        app.tab_bar_rect = Some(Rect { x: 0, y: 1, width: 60, height: 3 });
        let (tx, _rx) = test_playground_tx();

        handle_mouse(&mut app, left_click(20, 1), &tx);

        assert_eq!(app.active, PanelKind::Overview);
    }
}
