//! Background GitHub-release update check + staged binary swap.
//!
//! A background check never touches the running executable — it stages a
//! newer binary at a side path (`update_staged/` in the config dir), and
//! only the *next* process, before doing anything else, swaps it into place
//! via `self-replace`. This keeps a live monitoring session from ever being
//! disrupted by its own updater.
//!
//! `self_update` is configured with `default-features = false` +
//! `ureq`/`rustls` (see Cargo.toml) rather than its default `reqwest`
//! backend: `reqwest`'s `rustls` feature now hard-selects the `aws-lc-rs`
//! crypto provider (pulls in `aws-lc-sys`, which needs NASM), while `ureq`'s
//! `rustls` feature uses `ring` — matching this repo's existing
//! `rustls`/`tokio-postgres-rustls` backend choice (see CLAUDE.md's TLS
//! note) and keeping every release CI target buildable with no extra
//! toolchain.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::event::{AppEvent, StatusLevel};

const REPO_OWNER: &str = "kavin-vs";
const REPO_NAME: &str = "pgpilot";
const BIN_NAME: &str = "pgpilot";

/// Minimum time between checks, to avoid tripping GitHub's unauthenticated
/// API rate limit on frequent relaunches — the same class of problem
/// `install.sh` already hit once (see CLAUDE.md).
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Default, Serialize, Deserialize)]
struct UpdateState {
    #[serde(default)]
    last_checked_unix: Option<u64>,
    #[serde(default)]
    staged_version: Option<String>,
    /// Set when the most recent check attempt failed (network, rate limit,
    /// checksum mismatch, ...), cleared on the next successful attempt.
    /// Otherwise a persistent failure is indistinguishable from "not due
    /// yet" — this is the diagnostic trail for that case, kept out of the
    /// footer status line deliberately (see `update_check_task`'s doc).
    #[serde(default)]
    last_error: Option<String>,
}

fn config_dir() -> Result<PathBuf> {
    Ok(ProjectDirs::from("", "", "pgpilot")
        .context("could not determine home directory for config storage")?
        .config_dir()
        .to_path_buf())
}

fn state_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("update_state.toml"))
}

fn staged_bin_path() -> Result<PathBuf> {
    let name = if cfg!(windows) { "pgpilot.exe" } else { "pgpilot" };
    Ok(config_dir()?.join("update_staged").join(name))
}

/// Mirrors `config::load()`'s pattern exactly (empty default if missing).
fn load_state() -> Result<UpdateState> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(UpdateState::default());
    }
    let contents =
        std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))
}

/// Mirrors `config::save()`'s pattern exactly (0600 on unix).
fn save_state(state: &UpdateState) -> Result<()> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let contents = toml::to_string_pretty(state).context("failed to serialize update state")?;
    std::fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn due_for_check(state: &UpdateState) -> bool {
    match state.last_checked_unix {
        None => true,
        Some(last) => now_unix().saturating_sub(last) >= CHECK_INTERVAL.as_secs(),
    }
}

/// Blocking: checks GitHub for a newer release and, if found, downloads +
/// checksum-verifies it to `staged_bin_path()` (never the running
/// executable). Returns the new version string if one was newly staged.
///
/// `last_checked_unix` is persisted unconditionally, even when the network
/// call itself fails (e.g. rate-limited) — otherwise a persistent failure
/// would retry on every single launch instead of backing off, which is the
/// exact rate-limit-storm class of bug `install.sh` already hit once (see
/// CLAUDE.md).
fn check_and_stage_blocking() -> Result<Option<String>> {
    let mut state = load_state().unwrap_or_default();
    state.last_checked_unix = Some(now_unix());

    let outcome = fetch_and_stage(&mut state);
    state.last_error = outcome.as_ref().err().map(|e| format!("{e:#}"));
    let _ = save_state(&state);
    outcome
}

fn fetch_and_stage(state: &mut UpdateState) -> Result<Option<String>> {
    let checker = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(env!("CARGO_PKG_VERSION"))
        .no_confirm(true)
        .show_output(false)
        .show_download_progress(false)
        .build()?;

    let Some(release) = checker.is_update_available()? else {
        return Ok(None);
    };
    let new_version = release.version().to_string();

    let staged_path = staged_bin_path()?;
    if let Some(parent) = staged_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(env!("CARGO_PKG_VERSION"))
        .bin_install_path(&staged_path)
        .checksum_from_asset("checksums.txt")
        .no_confirm(true)
        .show_output(false)
        .show_download_progress(false)
        .build()?
        .update()?;

    state.staged_version = Some(new_version.clone());
    Ok(Some(new_version))
}

/// Spawned fire-and-forget from `main()`, same pattern as `poll_task`/
/// `playground_task`. Silent on any failure — a background update check
/// hiccupping on network shouldn't compete with real DB feedback on the
/// footer's one status line, and must never crash the app. Failures are
/// still recorded (see `UpdateState::last_error`) so they're not *invisible*,
/// just not disruptive. `force` (from `--force-update-check`) bypasses the
/// 24h throttle for this one launch only — the throttle-timestamp bookkeeping
/// itself is untouched.
pub async fn update_check_task(tx: mpsc::Sender<AppEvent>, force: bool) {
    // A corrupt/unreadable state file degrades to "never checked" rather
    // than disabling the updater forever — same recovery as
    // `check_and_stage_blocking` uses one function away.
    let state = load_state().unwrap_or_default();
    if !force && !due_for_check(&state) {
        return;
    }

    if let Ok(Ok(Some(version))) = tokio::task::spawn_blocking(check_and_stage_blocking).await {
        let _ = tx
            .send(AppEvent::Status(
                format!("Update v{version} ready — restart to apply"),
                StatusLevel::Info,
            ))
            .await;
    }
}

/// Swaps in a previously staged update, if one is present and still newer
/// than the binary currently running. Called first thing in `main()`,
/// before any terminal/DB setup — a fresh process is the only safe place to
/// replace its own executable file (mid-session would disrupt a live
/// monitoring TUI for no benefit).
pub fn apply_staged_update_if_present() -> Result<()> {
    let mut state = load_state()?;
    let Some(staged_version) = state.staged_version.clone() else {
        return Ok(());
    };

    let staged_path = staged_bin_path()?;
    let is_newer = self_update::version::bump_is_greater(env!("CARGO_PKG_VERSION"), &staged_version).unwrap_or(false);

    if !staged_path.exists() || !is_newer {
        // Stale/already-applied/downgrade marker — clear it defensively.
        state.staged_version = None;
        save_state(&state)?;
        let _ = std::fs::remove_file(&staged_path);
        return Ok(());
    }

    self_replace::self_replace(&staged_path)?;
    let _ = std::fs::remove_file(&staged_path);
    state.staged_version = None;
    save_state(&state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_for_check_when_never_checked() {
        let state = UpdateState::default();
        assert!(due_for_check(&state));
    }

    #[test]
    fn due_for_check_respects_throttle() {
        let fresh = UpdateState { last_checked_unix: Some(now_unix()), ..Default::default() };
        assert!(!due_for_check(&fresh));

        let stale = UpdateState {
            last_checked_unix: Some(now_unix() - CHECK_INTERVAL.as_secs() - 1),
            ..Default::default()
        };
        assert!(due_for_check(&stale));
    }

    #[test]
    fn corrupt_state_toml_falls_back_to_default_instead_of_erroring() {
        // Mirrors what `load_state()` would return for a corrupted
        // `update_state.toml` — `update_check_task` must treat this as
        // "never checked" (via `.unwrap_or_default()`), not permanently
        // disable itself.
        let parsed: Result<UpdateState, _> = toml::from_str("not valid toml {{{");
        assert!(parsed.is_err());
        let state = parsed.unwrap_or_default();
        assert!(due_for_check(&state));
    }
}
