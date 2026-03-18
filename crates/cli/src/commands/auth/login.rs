use anyhow::{bail, Context, Result};
use reqwest::Client;

use common::contract::{ProvisionRequest, ProvisionResponse};

use crate::config::Config;
use crate::oidc;
use crate::output::{OutputMode, print_ok};

pub async fn run(config: &mut Config, invite: Option<String>, output: OutputMode) -> Result<()> {
    let http = Client::new();

    let cli_cfg = oidc::fetch_cli_config(&http, config.api_url()).await?;
    let da = oidc::device_authorize(&http, &cli_cfg).await?;

    println!(
        "\nOpen this URL to authenticate:\n  {}\n\nWaiting for login...",
        da.verification_uri_complete
    );

    let tokens = oidc::poll_token(&http, &cli_cfg, &da).await?;
    let info = oidc::fetch_user_info(&http, &cli_cfg, &tokens).await?;

    print!("Provisioning account... ");
    let pr = provision(&http, config.api_url(), &info, invite.as_deref()).await?;
    println!("done.");

    let api_key = pr.api_key.ok_or_else(|| anyhow::anyhow!(
        "server did not return an API key — contact support or try again"
    ))?;
    config.token = Some(api_key);
    config.workspace_id = Some(pr.workspace_id);
    config.oidc_sub = pr.oidc_sub;
    config.oidc_client_id = Some(cli_cfg.client_id);
    config.oidc_device_auth_url = Some(cli_cfg.device_auth_url);
    config.oidc_token_url = Some(cli_cfg.token_url);
    config.oidc_userinfo_url = Some(cli_cfg.userinfo_url);
    config.oidc_revoke_url = cli_cfg.revoke_url;
    config.access_token = Some(tokens.access_token);
    config.save()?;

    print_ok(output, &format!(
        "\nWelcome, {}!\nConfig saved to ~/.config/flux/config.yaml",
        pr.name
    ));
    Ok(())
}

async fn provision(
    http: &Client,
    api_url: &str,
    info: &oidc::UserInfo,
    invite_code: Option<&str>,
) -> Result<ProvisionResponse> {
    let resp = http
        .post(format!("{}/auth/cli/provision", api_url))
        .json(&ProvisionRequest {
            email: info.email.clone(),
            first_name: info.given_name.clone(),
            last_name: info.family_name.clone(),
            oidc_sub: Some(info.sub.clone()),
            invite_code: invite_code.map(|s| s.to_string()),
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
