use std::time::Duration;

use anyhow::{Result, bail};
use reqwest::Client;
use serde::{Serialize, de::DeserializeOwned};

const MAX_RETRIES: u32 = 3;
const RETRY_BASE_MS: u64 = 500;

pub struct ResolvedService {
    pub id: String,
    pub slug: String,
}

pub struct ApiClient {
    client: Client,
    base_url: String,
    token: Option<String>,
}

/// Extract a human-readable error message from an API error response.
/// Tries to parse `{"error": "...", "message": "..."}` and falls back to
/// the raw status code if the body isn't structured JSON.
async fn api_error_message(resp: reqwest::Response) -> String {
    let status = resp.status();
    match resp.json::<serde_json::Value>().await {
        Ok(body) => {
            let msg = body.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
            if !msg.is_empty() {
                format!("{}: {} ({})", status.as_u16(), msg, code)
            } else {
                format!("{}", status.as_u16())
            }
        }
        Err(_) => format!("{}", status.as_u16()),
    }
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT
}

/// Exponential backoff with ±25% jitter to avoid thundering herd.
fn retry_delay(attempt: u32) -> Duration {
    let base = RETRY_BASE_MS * 2u64.pow(attempt);
    let jitter_range = base / 4;
    let jitter = if jitter_range > 0 {
        let mut buf = [0u8; 8];
        getrandom::getrandom(&mut buf).unwrap_or(());
        let rand_val = u64::from_ne_bytes(buf);
        (rand_val % (jitter_range * 2)).saturating_sub(jitter_range)
    } else {
        0
    };
    Duration::from_millis(base.wrapping_add(jitter))
}

impl ApiClient {
    pub fn new(base_url: &str, token: Option<&str>) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            base_url: base_url.to_string(),
            token: token.map(|t| t.to_string()),
        }
    }

    /// Core retry loop. Takes a closure that builds a fresh RequestBuilder each attempt.
    async fn send_with_retry(
        &self,
        build_request: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let mut last_err = None;
        for attempt in 0..MAX_RETRIES {
            match build_request().send().await {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) if !is_retryable_status(resp.status()) => {
                    bail!("{}", api_error_message(resp).await);
                }
                Ok(resp) => {
                    last_err = Some(api_error_message(resp).await);
                }
                Err(e) => {
                    last_err = Some(format!("request failed: {}", e));
                }
            }
            if attempt + 1 < MAX_RETRIES {
                tokio::time::sleep(retry_delay(attempt)).await;
            }
        }
        bail!("{}", last_err.expect("loop must set last_err before exhausting retries"))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth_header(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => req.header("X-Api-Key", token),
            None => req,
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .send_with_retry(|| self.auth_header(self.client.get(self.url(path))))
            .await?;
        Ok(resp.json().await?)
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let resp = self
            .send_with_retry(|| self.auth_header(self.client.post(self.url(path)).json(body)))
            .await?;
        Ok(resp.json().await?)
    }

    /// Resolve a service reference (slug or UUID) to id + slug.
    pub async fn resolve_service_full(&self, service_ref: &str) -> Result<ResolvedService> {
        let services: Vec<common::contract::ServiceResponse> = self.get("/services").await?;
        let matched = services
            .iter()
            .find(|s| s.slug == service_ref || s.id == service_ref)
            .ok_or_else(|| anyhow::anyhow!("no service {:?} found", service_ref))?;
        Ok(ResolvedService {
            id: matched.id.clone(),
            slug: matched.slug.clone(),
        })
    }

    /// Resolve a service reference (slug or UUID) to a UUID string.
    pub async fn resolve_service(&self, service_ref: &str) -> Result<String> {
        if uuid::Uuid::parse_str(service_ref).is_ok() {
            return Ok(service_ref.to_string());
        }
        Ok(self.resolve_service_full(service_ref).await?.id)
    }

    pub async fn post_no_body_with_response<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .send_with_retry(|| self.auth_header(self.client.post(self.url(path))))
            .await?;
        Ok(resp.json().await?)
    }

    pub async fn post_no_body(&self, path: &str) -> Result<()> {
        self.send_with_retry(|| self.auth_header(self.client.post(self.url(path))))
            .await?;
        Ok(())
    }

    /// Open a streaming GET (for SSE endpoints). Returns the raw response so
    /// the caller can consume `bytes_stream()`. No retry — SSE connections are
    /// long-lived and retrying mid-stream would replay events.
    pub async fn get_stream(&self, path: &str) -> Result<reqwest::Response> {
        let req = self
            .auth_header(
                self.client
                    .get(self.url(path))
                    .timeout(std::time::Duration::from_secs(400)), // longer than server 300s timeout
            )
            .build()?;
        let resp = self.client.execute(req).await?;
        if !resp.status().is_success() {
            bail!("{}", api_error_message(resp).await);
        }
        Ok(resp)
    }
}
