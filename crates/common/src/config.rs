/// Shared environment-based config helpers.
use anyhow::{Context, Result};

pub fn require_env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{} not set", key))
}

pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
