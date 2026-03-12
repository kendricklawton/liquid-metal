use anyhow::{bail, Result};
use reqwest::Client;
use serde::{de::DeserializeOwned, Serialize};

pub struct ApiClient {
    client: Client,
    base_url: String,
    token: Option<String>,
}

/// Extract a human-readable error message from an API error response.
/// Tries to parse `{"error": "...", "message": "..."}` and falls back to
/// the raw status code if the body isn't structured JSON.
async fn api_error_message(resp: reqwest::Response, method: &str, path: &str) -> String {
    let status = resp.status();
    match resp.json::<serde_json::Value>().await {
        Ok(body) => {
            let msg = body.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
            if !msg.is_empty() {
                format!("{} {} {}: {} ({})", status.as_u16(), method, path, msg, code)
            } else {
                format!("{} {} {}", status.as_u16(), method, path)
            }
        }
        Err(_) => format!("{} {} {}", status.as_u16(), method, path),
    }
}

impl ApiClient {
    pub fn new(base_url: &str, token: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            token: token.map(|t| t.to_string()),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut req = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(token) = &self.token {
            req = req.header("X-Api-Key", token);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            bail!("{}", api_error_message(resp, "GET", path).await);
        }
        Ok(resp.json().await?)
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let mut req = self.client.post(format!("{}{}", self.base_url, path)).json(body);
        if let Some(token) = &self.token {
            req = req.header("X-Api-Key", token);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            bail!("{}", api_error_message(resp, "POST", path).await);
        }
        Ok(resp.json().await?)
    }

    pub async fn post_admin<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        secret: &str,
    ) -> Result<T> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .header("X-Internal-Secret", secret)
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("{}", api_error_message(resp, "POST", path).await);
        }
        Ok(resp.json().await?)
    }

    pub async fn post_no_body(&self, path: &str) -> Result<()> {
        let mut req = self.client.post(format!("{}{}", self.base_url, path));
        if let Some(token) = &self.token {
            req = req.header("X-Api-Key", token);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            bail!("{}", api_error_message(resp, "POST", path).await);
        }
        Ok(())
    }
}
