use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    pub token: Option<String>,
    pub api_url: Option<String>,
    pub workspace_id: Option<String>,
    pub oidc_sub: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_device_auth_url: Option<String>,
    pub oidc_token_url: Option<String>,
    pub oidc_userinfo_url: Option<String>,
    pub oidc_revoke_url: Option<String>,
    pub access_token: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path()?;

        let mut cfg = if path.exists() {
            warn_if_permissive(&path);
            let contents = fs::read_to_string(&path).context("failed to read config file")?;
            serde_yaml::from_str(&contents).context("failed to parse config file")?
        } else {
            Self::default()
        };

        // Env vars override config file (12-factor: config in the environment)
        if let Ok(v) = std::env::var("FLUX_TOKEN") {
            cfg.token = Some(v);
        }
        if let Ok(v) = std::env::var("FLUX_ACCESS_TOKEN") {
            cfg.access_token = Some(v);
        }
        if let Ok(v) = std::env::var("API_URL") {
            cfg.api_url = Some(v);
        }
        if let Ok(v) = std::env::var("FLUX_WORKSPACE_ID") {
            cfg.workspace_id = Some(v);
        }

        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("failed to create config directory")?;
            restrict_permissions(parent, 0o700);
        }

        let contents = serde_yaml::to_string(self).context("failed to serialize config")?;
        fs::write(&path, &contents).context("failed to write config file")?;
        restrict_permissions(&path, 0o600);
        Ok(())
    }

    pub fn api_url(&self) -> &str {
        self.api_url
            .as_deref()
            .unwrap_or("https://api.liquidmetal.dev")
    }

    pub fn platform_domain(&self) -> &str {
        static DOMAIN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        DOMAIN.get_or_init(|| {
            std::env::var("FLUX_PLATFORM_DOMAIN").unwrap_or_else(|_| "liquidmetal.dev".to_string())
        })
    }

    pub fn require_token(&self) -> Result<&str> {
        match self.token.as_deref() {
            Some(t) => Ok(t),
            None => bail!("not logged in — run: flux login"),
        }
    }

    pub fn delete() -> Result<()> {
        let path = config_path()?;
        if path.exists() {
            fs::remove_file(&path).context("failed to delete config file")?;
        }
        Ok(())
    }
}

/// Config file path, respecting XDG and FLUX_CONFIG_FILE.
///
/// Priority: $FLUX_CONFIG_FILE → $XDG_CONFIG_HOME/flux/config.yaml → ~/.config/flux/config.yaml
pub fn config_path() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("FLUX_CONFIG_FILE") {
        return Ok(PathBuf::from(custom));
    }

    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .context("could not determine config directory")?;

    Ok(config_dir.join("flux").join("config.yaml"))
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path, _mode: u32) {}

#[cfg(unix)]
fn warn_if_permissive(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path) {
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            eprintln!(
                "warning: {} is accessible by other users (mode {:04o}); run 'chmod 600 {}'",
                path.display(),
                mode,
                path.display()
            );
        }
    }
}

#[cfg(not(unix))]
fn warn_if_permissive(_path: &std::path::Path) {}
