use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Saved connection info. Deliberately excludes the password — see CLAUDE.md
/// for why: profiles are resolved via PGPASSWORD or an interactive prompt
/// instead of ever persisting a secret to disk.
///
/// SSL fields use `#[serde(default)]` so profiles saved before this feature
/// existed still parse (missing fields become `false`/`None`, i.e. SSL off).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub dbname: String,
    #[serde(default)]
    pub ssl: bool,
    /// CA cert path to verify the server against. If SSL is on but this is
    /// unset, the connection is encrypted without verifying the server cert.
    #[serde(default)]
    pub ssl_root_cert: Option<String>,
    /// Client cert + key pair for mutual TLS. Both or neither.
    #[serde(default)]
    pub ssl_client_cert: Option<String>,
    #[serde(default)]
    pub ssl_client_key: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

fn config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "pgpilot")
        .context("could not determine home directory for config storage")?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// Loads the saved config, or an empty one if the file doesn't exist yet.
pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))
}

/// Writes the config, creating its parent directory if needed, and
/// restricting permissions to the owner (defense in depth, even though no
/// secrets are stored here today).
pub fn save(config: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let contents = toml::to_string_pretty(config).context("failed to serialize config")?;
    std::fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}
