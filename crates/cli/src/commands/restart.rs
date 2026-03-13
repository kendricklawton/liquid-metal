use anyhow::Result;
use serde::Deserialize;

use crate::client::ApiClient;
use crate::config::Config;

#[derive(Deserialize)]
struct Service {
    slug: String,
    status: String,
}

#[derive(Deserialize)]
struct RestartResponse {
    service: Service,
}

pub async fn run(config: &Config, service_ref: &str) -> Result<()> {
    let token = config.require_token()?;
    let client = ApiClient::new(config.api_url(), Some(token));

    let service_id = client.resolve_service(service_ref).await?;

    let resp: RestartResponse = client
        .post(&format!("/services/{}/restart", service_id), &serde_json::Value::Null)
        .await?;

    println!(
        "Restarting {} — status: {}",
        resp.service.slug, resp.service.status
    );
    Ok(())
}
