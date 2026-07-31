mod app;
mod cli;
mod config;
mod db;
mod diagnosis;
mod event;
mod format;
mod onboarding;
mod ui;

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use app::{App, PanelKind};
use clap::Parser;
use cli::{Cli, ConnParts};
use crossterm::event::{Event, EventStream, KeyCode};
use db::tls::TlsMode;
use db::PollControl;
use event::{AppEvent, PanelSnapshot, StatusLevel};
use futures_util::StreamExt;
use tokio::sync::{mpsc, Notify};

/// Fast-tier poll intervals cycled by `-`/`+`, matching the design mockup's
/// own list.
const RATE_OPTIONS_MS: [u64; 8] = [250, 500, 1000, 1500, 2000, 3000, 5000, 10000];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    tokio::spawn(db::poll_task(
        client,
        conninfo,
        tls,
        conn_parts,
        interval,
        tx,
        refresh_now.clone(),
        control_rx,
    ));

    let result = run(&mut terminal, &mut app, rx, refresh_now, control_tx).await;
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
) -> anyhow::Result<()> {
    let mut reader = EventStream::new();
    // Decoupled from the (slower) DB poll interval so the UI redraws
    // promptly on keystrokes and keeps "last updated" freshness visible.
    let mut render_tick = tokio::time::interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            maybe_event = reader.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    handle_key(app, key.code, &refresh_now, &control_tx);
                }
            }
            Some(event) = rx.recv() => {
                apply_event(app, event);
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

fn handle_key(app: &mut App, code: KeyCode, refresh_now: &Arc<Notify>, control_tx: &mpsc::Sender<PollControl>) {
    if app.error_detail_open {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('e')) {
            app.error_detail_open = false;
        }
        return;
    }

    if app.trigger_detail_open {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
            app.trigger_detail_open = false;
        }
        return;
    }

    if app.activity_detail_open {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
            app.activity_detail_open = false;
        }
        return;
    }

    if app.diagnosis_open {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('g')) {
            app.diagnosis_open = false;
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

    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('1') => app.active = PanelKind::Overview,
        KeyCode::Char('2') => app.active = PanelKind::Queries,
        KeyCode::Char('3') => app.active = PanelKind::Activity,
        KeyCode::Char('4') => app.active = PanelKind::TablesIndexes,
        KeyCode::Char('5') => app.active = PanelKind::Triggers,
        KeyCode::Char('d') => app.open_db_popup(),
        KeyCode::Char('e') if !app.errors.is_empty() => app.error_detail_open = true,
        KeyCode::Char('g') => app.diagnosis_open = true,
        KeyCode::Enter if app.active == PanelKind::Triggers && app.selected_trigger().is_some() => {
            app.trigger_detail_open = true;
        }
        KeyCode::Enter if app.active == PanelKind::Activity && app.selected_activity_row().is_some() => {
            app.activity_detail_open = true;
        }
        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
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

fn apply_event(app: &mut App, event: AppEvent) {
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
                PanelSnapshot::CachePerDatabase(data) => app.cache_per_database = Some(data),
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
        AppEvent::DbSwitched(name) => app.on_db_switched(name),
        AppEvent::ServerInfo(info) => app.server_info = Some(info),
        AppEvent::Status(text, level) => app.set_status(text, level),
    }
}
