use anyhow::{bail, Result};
use reqwest::Client;
use serde::{Serialize, de::DeserializeOwned};

// Re-export contract types so route handlers can use them directly.
pub use common::contract::{
    LogLineResponse as LogLine,
    ProjectResponse as Project,
    ProvisionRequest,
    ProvisionResponse,
    ServiceResponse as Service,
    UserResponse as User,
    WorkspaceResponse as Workspace,
};

/// HTTP client for calling the internal Rust API.
///
/// Uses `X-Internal-Secret` + `X-On-Behalf-Of` for user-scoped requests
/// (trusted internal service pattern). The web BFF never handles user tokens
/// directly — it acts on behalf of authenticated users via the internal secret.
pub struct ApiClient {
    client: Client,
    base_url: String,
    internal_secret: String,
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
    pub fn new(client: Client, base_url: &str, internal_secret: &str) -> Self {
        Self {
            client,
            base_url: base_url.to_string(),
            internal_secret: internal_secret.to_string(),
        }
    }

    /// GET on behalf of a user (X-Internal-Secret + X-On-Behalf-Of).
    pub async fn get<T: DeserializeOwned>(&self, path: &str, user_id: &str) -> Result<T> {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .header("X-Internal-Secret", &self.internal_secret)
            .header("X-On-Behalf-Of", user_id)
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("{}", api_error_message(resp, "GET", path).await);
        }
        Ok(resp.json().await?)
    }

    /// POST on behalf of a user (X-Internal-Secret + X-On-Behalf-Of).
    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        user_id: &str,
        body: &B,
    ) -> Result<T> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("X-Internal-Secret", &self.internal_secret)
            .header("X-On-Behalf-Of", user_id)
            .json(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("{}", api_error_message(resp, "POST", path).await);
        }
        Ok(resp.json().await?)
    }

    /// POST on behalf of a user, no response body (204 expected).
    pub async fn post_no_body(&self, path: &str, user_id: &str) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("X-Internal-Secret", &self.internal_secret)
            .header("X-On-Behalf-Of", user_id)
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("{}", api_error_message(resp, "POST", path).await);
        }
        Ok(())
    }

    /// POST with internal secret only (for provisioning — no user context).
    pub async fn post_internal<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("X-Internal-Secret", &self.internal_secret)
            .json(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("{}", api_error_message(resp, "POST", path).await);
        }
        Ok(resp.json().await?)
    }
}
