use anyhow::Result;
use reqwest::Client;

use crate::config::Config;

pub async fn run(config: &mut Config) -> Result<()> {
    let path = crate::config::config_path()?;

    if !path.exists() {
        println!("Not logged in.");
        return Ok(());
    }

    revoke_token(config).await;
    std::fs::remove_file(&path)?;
    println!("Logged out.");
    Ok(())
}

async fn revoke_token(config: &Config) {
    let (revoke_url, client_id, token) = match (
        config.oidc_revoke_url.as_deref(),
        config.oidc_client_id.as_deref(),
        config.access_token.as_deref(),
    ) {
        (Some(u), Some(c), Some(t)) => (u, c, t),
        _ => return, // revocation is best-effort; skip if any value is missing
    };

    let _ = Client::new()
        .post(revoke_url)
        .form(&[("token", token), ("client_id", client_id)])
        .send()
        .await;
}
