use std::io::{self, Write};

use anyhow::{bail, Result};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
}

/// Print structured data. JSON mode serializes directly; human mode calls the closure.
pub fn print_data<T: Serialize>(mode: OutputMode, data: &T, human: impl FnOnce(&T)) {
    match mode {
        OutputMode::Json => println!("{}", serde_json::to_string(data).expect("serializable types must produce valid JSON")),
        OutputMode::Human => human(data),
    }
}

/// Print success for action-only commands.
pub fn print_ok(mode: OutputMode, message: &str) {
    match mode {
        OutputMode::Json => println!(r#"{{"ok":true}}"#),
        OutputMode::Human => println!("{}", message),
    }
}

/// Prompt the user for confirmation. Returns Ok(()) if confirmed, error if not.
/// In JSON mode, confirmations are auto-skipped (agents can't read interactive prompts).
pub fn confirm(mode: OutputMode, message: &str) -> Result<()> {
    if mode == OutputMode::Json {
        return Ok(());
    }
    print!("{} [y/N]: ", message);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        bail!("Aborted.")
    }
}

/// Print an error to stderr.
pub fn print_error(mode: OutputMode, error: &anyhow::Error) {
    match mode {
        OutputMode::Json => {
            eprintln!("{}", serde_json::json!({"error": format!("{error:#}")}));
        }
        OutputMode::Human => {
            eprintln!("error: {error}");
            if let Some(hint) = error_suggestion(&format!("{error:#}")) {
                eprintln!("\n  hint: {hint}");
            }
        }
    }
}

/// Map common API errors to actionable next-step suggestions.
fn error_suggestion(msg: &str) -> Option<&'static str> {
    let lower = msg.to_lowercase();
    if lower.contains("not found") || lower.contains("no service") {
        Some("run 'flux services' to see available services")
    } else if lower.contains("401") || lower.contains("unauthorized") || lower.contains("not logged in") {
        Some("run 'flux login' to authenticate")
    } else if lower.contains("api key has expired") {
        Some("run 'flux login' to get a new API key")
    } else if lower.contains("already stopped") {
        Some("run 'flux restart <service>' to bring it back up")
    } else if lower.contains("service_limit") {
        Some("delete unused services with 'flux delete <service>'")
    } else if lower.contains("liquid-metal.toml") {
        Some("run 'flux init' to set up this directory")
    } else if lower.contains("insufficient_scope") {
        Some("your API key may lack the required scope — run 'flux login' for a new key")
    } else {
        None
    }
}

/// Format a byte count as a human-readable string (e.g., "1.2 MB").
pub fn human_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Format an ISO 8601 / Postgres timestamp as a relative age (e.g., "3d", "2h", "45m").
/// Falls back to the raw string on parse failure.
pub fn relative_time(iso: &str) -> String {
    let trimmed = iso.trim();
    let Some(ts) = parse_timestamp_secs(trimmed) else {
        return trimmed.to_string();
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let delta = now - ts;
    if delta < 0 {
        return "now".to_string();
    }

    let mins = delta / 60;
    let hours = mins / 60;
    let days = hours / 24;

    if days > 0 {
        format!("{}d", days)
    } else if hours > 0 {
        format!("{}h", hours)
    } else if mins > 0 {
        format!("{}m", mins)
    } else {
        "<1m".to_string()
    }
}

/// Best-effort parse of a Postgres/RFC3339 timestamp to Unix epoch seconds (UTC).
fn parse_timestamp_secs(s: &str) -> Option<i64> {
    let s = s.replace('T', " ");
    let date_time = s.get(..19)?;

    let year: i64 = date_time.get(0..4)?.parse().ok()?;
    let month: i64 = date_time.get(5..7)?.parse().ok()?;
    let day: i64 = date_time.get(8..10)?.parse().ok()?;
    let hour: i64 = date_time.get(11..13)?.parse().ok()?;
    let min: i64 = date_time.get(14..16)?.parse().ok()?;
    let sec: i64 = date_time.get(17..19)?.parse().ok()?;

    let mut total_days: i64 = 0;
    for y in 1970..year {
        total_days += if is_leap(y) { 366 } else { 365 };
    }
    let month_days = [31, if is_leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 0..(month - 1) as usize {
        total_days += month_days.get(m).copied().unwrap_or(30) as i64;
    }
    total_days += day - 1;

    Some(total_days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Apply ANSI color codes to a log line based on severity keywords.
/// Only call in human mode when stdout is a terminal.
pub fn colorize_log(line: &str) -> String {
    let upper = line.to_uppercase();
    if upper.contains("ERROR") || upper.contains("FATAL") || upper.contains("PANIC") {
        format!("\x1b[31m{}\x1b[0m", line)
    } else if upper.contains("WARN") {
        format!("\x1b[33m{}\x1b[0m", line)
    } else if upper.contains("DEBUG") || upper.contains("TRACE") {
        format!("\x1b[2m{}\x1b[0m", line)
    } else {
        line.to_string()
    }
}
