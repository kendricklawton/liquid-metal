use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
pub struct CliConfig {
    pub client_id: String,
    pub device_auth_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub revoke_url: Option<String>,
}

#[derive(Deserialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub verification_uri_complete: String,
    pub interval: u64,
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
}

#[derive(Deserialize)]
pub struct UserInfo {
    pub sub: String,
    pub email: String,
    pub given_name: String,
    pub family_name: String,
}

pub async fn fetch_cli_config(http: &Client, api_url: &str) -> Result<CliConfig> {
    let cfg: CliConfig = http
        .get(format!("{}/auth/cli/config", api_url))
        .send()
        .await
        .context("fetch CLI config")?
        .json()
        .await
        .context("decode CLI config")?;
    if cfg.client_id.is_empty() || cfg.device_auth_url.is_empty() {
        bail!("server returned incomplete config");
    }
    Ok(cfg)
}

pub async fn device_authorize(http: &Client, cfg: &CliConfig) -> Result<DeviceAuthResponse> {
    let resp = http
        .post(&cfg.device_auth_url)
        .form(&[("client_id", cfg.client_id.as_str()), ("scope", "openid profile email")])
        .send()
        .await
        .context("device authorization")?;
    if !resp.status().is_success() {
        bail!("auth provider returned {}", resp.status());
    }
    Ok(resp.json().await?)
}

pub async fn poll_token(
    http: &Client,
    cfg: &CliConfig,
    da: &DeviceAuthResponse,
) -> Result<TokenResponse> {
    let mut interval = Duration::from_secs(da.interval.max(5));
    let deadline = Instant::now() + Duration::from_secs(300);

    while Instant::now() < deadline {
        tokio::time::sleep(interval).await;

        let resp = http
            .post(&cfg.token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", da.device_code.as_str()),
                ("client_id", cfg.client_id.as_str()),
            ])
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(resp.json().await?);
        }

        #[derive(Deserialize)]
        struct ErrResp {
            error: String,
        }
        let err: ErrResp = resp.json().await?;
        match err.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += Duration::from_secs(5),
            "access_denied" => bail!("login cancelled"),
            other => bail!("unexpected error from auth provider: {:?}", other),
        }
    }

    bail!("login timed out")
}

pub async fn fetch_user_info(http: &Client, cfg: &CliConfig, tokens: &TokenResponse) -> Result<UserInfo> {
    let info: UserInfo = http
        .get(&cfg.userinfo_url)
        .bearer_auth(&tokens.access_token)
        .send()
        .await
        .context("fetch user info")?
        .json()
        .await?;
    if info.email.is_empty() {
        bail!("no email in userinfo response");
    }
    Ok(info)
}
