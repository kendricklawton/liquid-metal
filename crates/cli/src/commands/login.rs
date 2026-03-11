use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::config::Config;

pub async fn run(config: &mut Config, invite: Option<String>) -> Result<()> {
    let http = Client::new();

    let cli_cfg = fetch_cli_config(&http, config.api_url()).await?;
    let da = device_authorize(&http, &cli_cfg.zitadel_domain, &cli_cfg.zitadel_client_id).await?;

    println!(
        "\nOpen this URL to authenticate:\n  {}\n\nWaiting for login...",
        da.verification_uri_complete
    );

    let tokens = poll_token(
        &http,
        &cli_cfg.zitadel_domain,
        &cli_cfg.zitadel_client_id,
        &da.device_code,
        da.interval,
    )
    .await?;

    let info = fetch_user_info(&http, &cli_cfg.zitadel_domain, &tokens.access_token).await?;

    print!("Provisioning account... ");
    let pr = provision(&http, config.api_url(), &info, invite.as_deref()).await?;
    println!("done.");

    config.token = Some(pr.id);
    config.workspace_id = Some(pr.workspace_id);
    config.zitadel_sub = Some(pr.zitadel_sub);
    config.zitadel_domain = Some(cli_cfg.zitadel_domain);
    config.zitadel_client_id = Some(cli_cfg.zitadel_client_id);
    config.access_token = Some(tokens.access_token);
    config.save()?;

    println!(
        "\nWelcome, {}!\nConfig saved to ~/.config/flux/config.yaml",
        pr.name
    );
    Ok(())
}

#[derive(Deserialize)]
struct CliConfig {
    zitadel_domain: String,
    zitadel_client_id: String,
}

async fn fetch_cli_config(http: &Client, api_url: &str) -> Result<CliConfig> {
    let cfg: CliConfig = http
        .get(format!("{}/auth/cli/config", api_url))
        .send()
        .await
        .context("fetch CLI config")?
        .json()
        .await
        .context("decode CLI config")?;
    if cfg.zitadel_domain.is_empty() || cfg.zitadel_client_id.is_empty() {
        bail!("server returned incomplete config");
    }
    Ok(cfg)
}

#[derive(Deserialize)]
struct DeviceAuthResponse {
    device_code: String,
    verification_uri_complete: String,
    interval: u64,
}

async fn device_authorize(http: &Client, domain: &str, client_id: &str) -> Result<DeviceAuthResponse> {
    let resp = http
        .post(format!("https://{}/oauth/v2/device_authorization", domain))
        .form(&[("client_id", client_id), ("scope", "openid profile email")])
        .send()
        .await
        .context("device authorization")?;
    if !resp.status().is_success() {
        bail!("Zitadel returned {}", resp.status());
    }
    Ok(resp.json().await?)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

async fn poll_token(
    http: &Client,
    domain: &str,
    client_id: &str,
    device_code: &str,
    interval_secs: u64,
) -> Result<TokenResponse> {
    let mut interval = Duration::from_secs(interval_secs.max(5));
    let deadline = Instant::now() + Duration::from_secs(300);

    while Instant::now() < deadline {
        tokio::time::sleep(interval).await;

        let resp = http
            .post(format!("https://{}/oauth/v2/token", domain))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", client_id),
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
            other => bail!("unexpected error from Zitadel: {:?}", other),
        }
    }

    bail!("login timed out")
}

#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    email: String,
    given_name: String,
    family_name: String,
}

async fn fetch_user_info(http: &Client, domain: &str, access_token: &str) -> Result<UserInfo> {
    let info: UserInfo = http
        .get(format!("https://{}/oidc/v1/userinfo", domain))
        .bearer_auth(access_token)
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

#[derive(Serialize)]
struct ProvisionRequest<'a> {
    email: &'a str,
    first_name: &'a str,
    last_name: &'a str,
    zitadel_sub: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    invite_code: Option<&'a str>,
}

#[derive(Deserialize)]
struct ProvisionResponse {
    id: String,
    name: String,
    workspace_id: String,
    zitadel_sub: String,
}

async fn provision(
    http: &Client,
    api_url: &str,
    info: &UserInfo,
    invite_code: Option<&str>,
) -> Result<ProvisionResponse> {
    let resp = http
        .post(format!("{}/auth/cli/provision", api_url))
        .json(&ProvisionRequest {
            email: &info.email,
            first_name: &info.given_name,
            last_name: &info.family_name,
            zitadel_sub: &info.sub,
            invite_code,
        })
        .send()
        .await
        .context("provision request")?;

    match resp.status().as_u16() {
        200 => {}
        403 => bail!("invalid or missing invite code — run: flux login --invite <code>"),
        code => bail!("API returned {}", code),
    }

    let pr: ProvisionResponse = resp.json().await.context("decode provision response")?;
    if pr.id.is_empty() {
        bail!("no user ID in provision response");
    }
    Ok(pr)
}
