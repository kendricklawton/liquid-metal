use anyhow::{bail, Result};
use reqwest::Client;
use serde::{de::DeserializeOwned, Serialize};

pub struct ApiClient {
    client: Client,
    base_url: String,
    token: Option<String>,
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
            bail!("API error {} on GET {}", resp.status(), path);
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
            bail!("API error {} on POST {}", resp.status(), path);
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
        if resp.status().as_u16() == 403 {
            bail!("wrong FLUX_ADMIN_SECRET");
        }
        if !resp.status().is_success() {
            bail!("API error {} on POST {}", resp.status(), path);
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
            bail!("API error {} on POST {}", resp.status(), path);
        }
        Ok(())
    }
}
