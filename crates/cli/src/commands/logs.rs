use anyhow::Result;
use serde::Deserialize;

use crate::client::ApiClient;
use crate::config::Config;

#[derive(Deserialize)]
struct LogLine {
    ts: Option<String>,
    message: String,
}

pub async fn run(config: &Config, service_id: &str, limit: i32) -> Result<()> {
    let token = config.require_token()?;
    let client = ApiClient::new(config.api_url(), Some(token));

    let lines: Vec<LogLine> = client
        .get(&format!("/services/{}/logs?limit={}", service_id, limit))
        .await?;

    if lines.is_empty() {
        println!("no log lines found");
        return Ok(());
    }

    for l in &lines {
        match &l.ts {
            Some(ts) => println!("[{}] {}", ts, l.message),
            None => println!("{}", l.message),
        }
    }
    Ok(())
}
