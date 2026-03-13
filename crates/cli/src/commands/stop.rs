use anyhow::Result;

use crate::client::ApiClient;
use crate::config::Config;

pub async fn run(config: &Config, service_ref: &str) -> Result<()> {
    let token = config.require_token()?;
    let client = ApiClient::new(config.api_url(), Some(token));

    let service_id = client.resolve_service(service_ref).await?;

    client
        .post_no_body(&format!("/services/{}/stop", service_id))
        .await?;
    println!("Service {} stopped.", service_ref);
    Ok(())
}
