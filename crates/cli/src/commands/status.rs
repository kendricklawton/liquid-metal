use anyhow::Result;
use serde::Deserialize;

use crate::client::ApiClient;
use crate::config::Config;

#[derive(Deserialize)]
struct Service {
    name: String,
    engine: String,
    status: String,
    upstream_addr: String,
}

pub async fn run(config: &Config) -> Result<()> {
    let token = config.require_token()?;
    let client = ApiClient::new(config.api_url(), Some(token));

    let services: Vec<Service> = client.get("/services").await?;
    if services.is_empty() {
        println!("no services found");
        return Ok(());
    }

    println!("{:<20} {:<10} {:<15} {}", "NAME", "ENGINE", "STATUS", "UPSTREAM");
    for s in &services {
        println!("{:<20} {:<10} {:<15} {}", s.name, s.engine, s.status, s.upstream_addr);
    }
    Ok(())
}
