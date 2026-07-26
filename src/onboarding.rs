use std::io::{self, Write};

use anyhow::{Context, Result};

use crate::config::{self, Config, Profile};

pub enum PickResult {
    Existing(Profile),
    New(Profile),
    Quit,
}

fn prompt_line(prompt: &str, default: Option<&str>) -> Result<String> {
    match default {
        Some(d) => print!("{prompt} [{d}]: "),
        None => print!("{prompt}: "),
    }
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    if input.is_empty() {
        Ok(default.unwrap_or("").to_string())
    } else {
        Ok(input.to_string())
    }
}

fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool> {
    let default_label = if default { "y" } else { "n" };
    loop {
        let answer = prompt_line(&format!("{prompt} (y/n)"), Some(default_label))?;
        match answer.to_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please answer y or n."),
        }
    }
}

/// Blank input keeps `current` as-is (including if it was already `None`);
/// `-` explicitly clears it; anything else becomes the new value.
fn prompt_optional_path(prompt: &str, current: Option<&str>) -> Result<Option<String>> {
    let shown = current.unwrap_or("none");
    let input = prompt_line(&format!("{prompt} ('-' to clear)"), Some(shown))?;
    match input.as_str() {
        "-" => Ok(None),
        v if v == shown && current.is_none() => Ok(None),
        v => Ok(Some(v.to_string())),
    }
}

/// Prompts for host/port/user/dbname/SSL settings. `existing` pre-fills
/// current values (for editing); `None` means fresh defaults (for adding).
fn prompt_profile_fields(existing: Option<&Profile>) -> Result<Profile> {
    let default_user = std::env::var("USER").unwrap_or_else(|_| "postgres".to_string());

    let host = prompt_line(
        "Host",
        Some(existing.map_or("localhost", |p| p.host.as_str())),
    )?;
    let port: u16 = loop {
        let default_port = existing.map_or(5432, |p| p.port).to_string();
        let p = prompt_line("Port", Some(&default_port))?;
        match p.parse() {
            Ok(v) => break v,
            Err(_) => println!("Invalid port, please enter a number."),
        }
    };
    let user = prompt_line(
        "User",
        Some(existing.map_or(default_user.as_str(), |p| p.user.as_str())),
    )?;
    let dbname = prompt_line(
        "Database",
        Some(existing.map_or(user.as_str(), |p| p.dbname.as_str())),
    )?;

    let ssl = prompt_yes_no("Use SSL?", existing.is_some_and(|p| p.ssl))?;

    let (ssl_root_cert, ssl_client_cert, ssl_client_key) = if ssl {
        let root = prompt_optional_path(
            "CA root cert path",
            existing.and_then(|p| p.ssl_root_cert.as_deref()),
        )?;
        let cert = prompt_optional_path(
            "Client cert path",
            existing.and_then(|p| p.ssl_client_cert.as_deref()),
        )?;
        let key = prompt_optional_path(
            "Client key path",
            existing.and_then(|p| p.ssl_client_key.as_deref()),
        )?;
        (root, cert, key)
    } else {
        (None, None, None)
    };

    Ok(Profile {
        host,
        port,
        user,
        dbname,
        ssl,
        ssl_root_cert,
        ssl_client_cert,
        ssl_client_key,
    })
}

/// Prompts for a name plus all connection fields, saves the result into
/// `config`, and returns it. Run as plain stdin/stdout I/O, before
/// `ratatui::init()`.
pub fn run_wizard(config: &mut Config) -> Result<Profile> {
    println!("Add a new connection:");

    let name = loop {
        let n = prompt_line("Name", None)?;
        if !n.is_empty() {
            break n;
        }
        println!("Name can't be empty.");
    };

    let profile = prompt_profile_fields(None)?;

    config.profiles.insert(name, profile.clone());
    config::save(config)?;

    Ok(profile)
}

/// Re-prompts all connection fields for an existing profile, pre-filled with
/// its current values, and saves the changes in place.
pub fn edit_profile(config: &mut Config, name: &str) -> Result<Profile> {
    let existing = config
        .profiles
        .get(name)
        .cloned()
        .with_context(|| format!("no saved connection named '{name}'"))?;

    println!("Editing '{name}' (blank keeps the current value):");
    let profile = prompt_profile_fields(Some(&existing))?;

    config.profiles.insert(name.to_string(), profile.clone());
    config::save(config)?;

    Ok(profile)
}

fn print_profile_list(config: &Config, names: &[String]) {
    println!("Saved connections:");
    for (i, name) in names.iter().enumerate() {
        let p = &config.profiles[name];
        let ssl_tag = if p.ssl { " [SSL]" } else { "" };
        println!(
            "  {}) {}  ({}:{}/{}){ssl_tag}",
            i + 1,
            name,
            p.host,
            p.port,
            p.dbname
        );
    }
    println!("  e) Edit a connection");
    println!("  n) Add new connection");
    println!("  q) Quit");
}

/// Shows saved profiles (if any) and lets the user pick one, edit one, add a
/// new one, or quit. If no profiles are saved yet, skips straight to the wizard.
pub fn list_and_pick(config: &mut Config) -> Result<PickResult> {
    if config.profiles.is_empty() {
        println!("No saved connections yet.");
        return Ok(PickResult::New(run_wizard(config)?));
    }

    let names: Vec<String> = config.profiles.keys().cloned().collect();
    print_profile_list(config, &names);

    loop {
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.eq_ignore_ascii_case("q") {
            return Ok(PickResult::Quit);
        }
        if input.eq_ignore_ascii_case("n") {
            return Ok(PickResult::New(run_wizard(config)?));
        }
        if input.eq_ignore_ascii_case("e") {
            print!("Edit which connection? > ");
            io::stdout().flush()?;

            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;
            let choice = choice.trim();

            if let Ok(idx) = choice.parse::<usize>()
                && idx >= 1
                && idx <= names.len()
            {
                let profile = edit_profile(config, &names[idx - 1])?;
                return Ok(PickResult::Existing(profile));
            }
            println!("Invalid choice, try again.");
            continue;
        }
        if let Ok(idx) = input.parse::<usize>()
            && idx >= 1
            && idx <= names.len()
        {
            let profile = config.profiles[&names[idx - 1]].clone();
            return Ok(PickResult::Existing(profile));
        }

        println!("Invalid choice, try again.");
    }
}

/// Only called when `PGPASSWORD` isn't set. Empty input means "no password"
/// (e.g. trust/peer auth) rather than an empty-string password.
pub fn prompt_password() -> Option<String> {
    rpassword::prompt_password("Password: ")
        .ok()
        .filter(|p| !p.is_empty())
}
