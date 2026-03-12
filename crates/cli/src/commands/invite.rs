use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::client::ApiClient;
use crate::config::Config;

#[derive(Serialize)]
struct GenerateRequest {
    count: u32,
}

#[derive(Deserialize)]
struct GenerateResponse {
    codes: Vec<String>,
}

pub async fn run_generate(config: &Config, count: u32) -> Result<()> {
    let secret = std::env::var("FLUX_ADMIN_SECRET")
        .unwrap_or_default();
    if secret.is_empty() {
        bail!("FLUX_ADMIN_SECRET is not set");
    }

    let client = ApiClient::new(config.api_url(), None);
    let resp: GenerateResponse = client
        .post_admin("/admin/invites", &GenerateRequest { count }, &secret)
        .await?;

    for code in &resp.codes {
        println!("{}", code);
    }
    Ok(())
}
