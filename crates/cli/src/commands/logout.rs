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
    let (domain, client_id, token) = match (
        config.zitadel_domain.as_deref(),
        config.zitadel_client_id.as_deref(),
        config.access_token.as_deref(),
    ) {
        (Some(d), Some(c), Some(t)) => (d, c, t),
        _ => return,
    };

    let _ = Client::new()
        .post(format!("https://{}/oauth/v2/revoke", domain))
        .form(&[("token", token), ("client_id", client_id)])
        .send()
        .await;
}
