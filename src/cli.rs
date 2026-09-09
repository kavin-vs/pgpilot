use clap::Parser;

use crate::db::tls::TlsMode;

#[derive(Parser, Debug)]
#[command(name = "pgpilot", about = "Live TUI dashboard for Postgres usage/utilization")]
pub struct Cli {
    /// Full connection string, overrides all other connection flags
    #[arg(long)]
    pub dsn: Option<String>,

    #[arg(long, env = "PGHOST")]
    pub host: Option<String>,

    #[arg(long, env = "PGPORT")]
    pub port: Option<u16>,

    #[arg(long, env = "PGUSER")]
    pub user: Option<String>,

    #[arg(long, env = "PGDATABASE")]
    pub dbname: Option<String>,

    /// Named saved connection to use directly, skipping the picker
    #[arg(long)]
    pub profile: Option<String>,

    /// Use SSL for this connection (only applies to --host/--user/etc, not --dsn)
    #[arg(long)]
    pub ssl: bool,

    /// CA cert (PEM) to verify the server against; if omitted, SSL still
    /// encrypts but doesn't verify the server certificate
    #[arg(long)]
    pub ssl_root_cert: Option<String>,

    /// Client certificate (PEM), for mutual TLS — requires --ssl-client-key too
    #[arg(long)]
    pub ssl_client_cert: Option<String>,

    /// Client private key (PEM), for mutual TLS — requires --ssl-client-cert too
    #[arg(long)]
    pub ssl_client_key: Option<String>,

    /// Refresh interval in seconds
    #[arg(long, default_value_t = 2)]
    pub interval: u64,

    /// Use plain ASCII glyphs instead of Unicode block/braille characters,
    /// for terminals/fonts that don't render them cleanly
    #[arg(long)]
    pub ascii: bool,

    /// Skip the background check for a newer release on GitHub
    #[arg(long)]
    pub no_update_check: bool,

    /// Check for a newer release now, bypassing the 24h throttle
    #[arg(long)]
    pub force_update_check: bool,
}

impl Cli {
    /// True when the user gave enough on the command line (flags or libpq
    /// env vars) to connect directly, without the saved-profile picker.
    pub fn has_explicit_connection_info(&self) -> bool {
        self.dsn.is_some()
            || self.host.is_some()
            || self.port.is_some()
            || self.user.is_some()
            || self.dbname.is_some()
    }

    /// Builds a libpq-style connection string from flags/env vars, applying
    /// the same defaults (localhost/5432/$USER/same-as-user) used before
    /// saved profiles existed. `--dsn` overrides everything else.
    pub fn connection_string(&self) -> String {
        if let Some(dsn) = &self.dsn {
            return dsn.clone();
        }
        let parts = self.conn_parts().expect("conn_parts is Some when --dsn isn't set");
        build_conninfo(
            &parts.host,
            parts.port,
            &parts.user,
            &parts.dbname,
            parts.password.as_deref(),
        )
    }

    /// Host/port/user/dbname/password resolved from flags/env, with the same
    /// defaults as `connection_string()`. `None` when `--dsn` was given: an
    /// arbitrary DSN string can't be safely taken apart, so switching to a
    /// different database (which needs to rebuild a conninfo with a new
    /// `dbname`) isn't supported in that mode.
    pub fn conn_parts(&self) -> Option<ConnParts> {
        if self.dsn.is_some() {
            return None;
        }
        let user = self
            .user
            .clone()
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "postgres".to_string());
        let dbname = self.dbname.clone().unwrap_or_else(|| user.clone());
        let host = self.host.clone().unwrap_or_else(|| "localhost".to_string());
        let port = self.port.unwrap_or(5432);

        Some(ConnParts {
            host,
            port,
            user,
            password: std::env::var("PGPASSWORD").ok(),
            dbname,
        })
    }

    /// TLS settings from `--ssl*` flags. Not consulted for `--dsn` — see
    /// `TlsMode`'s doc comment for why.
    pub fn tls_mode(&self) -> TlsMode {
        TlsMode::from_parts(
            self.ssl,
            self.ssl_root_cert.clone(),
            self.ssl_client_cert.clone(),
            self.ssl_client_key.clone(),
        )
    }
}

/// Host/port/user/password, kept apart from `dbname` so a live connection
/// can be rebuilt against a different database (see `db::poll_task`'s
/// database-switching, triggered from the 'd' popup).
#[derive(Debug, Clone)]
pub struct ConnParts {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub dbname: String,
}

/// Shared libpq keyword/value conninfo string builder, used both by the
/// explicit-flags path above and by the saved-profile/onboarding path in
/// `main.rs`.
pub fn build_conninfo(host: &str, port: u16, user: &str, dbname: &str, password: Option<&str>) -> String {
    let mut parts = vec![
        format!("host={}", quote_conninfo_value(host)),
        format!("port={port}"),
        format!("user={}", quote_conninfo_value(user)),
        format!("dbname={}", quote_conninfo_value(dbname)),
    ];

    if let Some(password) = password {
        parts.push(format!("password={}", quote_conninfo_value(password)));
    }

    parts.join(" ")
}

/// Quotes a libpq keyword/value conninfo value so spaces, single quotes, and
/// backslashes (all significant to libpq's parser) can't corrupt the string
/// or run into the next keyword — always wrapped in single quotes, which is
/// valid even for values that didn't strictly need it.
fn quote_conninfo_value(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for c in value.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '\'' => quoted.push_str("\\'"),
            other => quoted.push(other),
        }
    }
    quoted.push('\'');
    quoted
}
