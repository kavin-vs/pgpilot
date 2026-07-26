const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

pub fn human_bytes(bytes: i64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit_idx = 0;
    while value >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }

    format!("{:.1} {}", value, UNITS[unit_idx])
}

/// Formats a duration given in seconds as a compact human string, same
/// coarsest-readable-unit convention as `human_bytes`.
pub fn human_duration(secs: f64) -> String {
    if secs < 1.0 {
        return format!("{} ms", (secs * 1000.0).round() as i64);
    }
    if secs < 60.0 {
        return format!("{secs:.1} s");
    }
    if secs < 3600.0 {
        return format!("{} min", (secs / 60.0).round() as i64);
    }
    if secs < 86400.0 {
        return format!("{:.1} h", secs / 3600.0);
    }
    format!("{:.1} d", secs / 86400.0)
}

/// Formats a poll interval for display in the header/footer/status line.
pub fn human_rate(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}
