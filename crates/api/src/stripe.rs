//! Thin Stripe HTTP client wrapping the REST API via reqwest.
//!
//! Uses form-encoded bodies (Stripe's native format) and Bearer auth.
//! No SDK dependency — just the 6 endpoints we need.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const BASE_URL: &str = "https://api.stripe.com/v1";

/// Minimal Stripe client. Holds the secret key and a shared reqwest client.
pub struct StripeClient {
    secret_key: String,
    http:       reqwest::Client,
}

impl StripeClient {
    pub fn new(secret_key: String, http: reqwest::Client) -> Self {
        Self { secret_key, http }
    }

    /// Create a Stripe Customer for a workspace.
    pub async fn create_customer(&self, name: &str, email: &str, workspace_id: &str) -> Result<Customer> {
        let resp = self.http
            .post(format!("{BASE_URL}/customers"))
            .bearer_auth(&self.secret_key)
            .form(&[
                ("name", name),
                ("email", email),
                ("metadata[workspace_id]", workspace_id),
            ])
            .send()
            .await
            .context("Stripe: create customer request")?;

        parse_response(resp).await.context("Stripe: create customer")
    }

    /// Create a Stripe Checkout Session for subscription (Pro/Team).
    pub async fn create_checkout_session(
        &self,
        customer_id: &str,
        price_id: &str,
        success_url: &str,
        cancel_url: &str,
    ) -> Result<CheckoutSession> {
        let resp = self.http
            .post(format!("{BASE_URL}/checkout/sessions"))
            .bearer_auth(&self.secret_key)
            .form(&[
                ("customer", customer_id),
                ("mode", "subscription"),
                ("line_items[0][price]", price_id),
                ("line_items[0][quantity]", "1"),
                ("success_url", success_url),
                ("cancel_url", cancel_url),
            ])
            .send()
            .await
            .context("Stripe: create checkout session request")?;

        parse_response(resp).await.context("Stripe: create checkout session")
    }

    /// Create a Stripe Checkout Session for a one-time top-up payment.
    pub async fn create_topup_session(
        &self,
        customer_id: &str,
        amount_cents: u64,
        success_url: &str,
        cancel_url: &str,
    ) -> Result<CheckoutSession> {
        let amount = amount_cents.to_string();
        let resp = self.http
            .post(format!("{BASE_URL}/checkout/sessions"))
            .bearer_auth(&self.secret_key)
            .form(&[
                ("customer", customer_id),
                ("mode", "payment"),
                ("line_items[0][price_data][currency]", "usd"),
                ("line_items[0][price_data][product_data][name]", "Compute Credit Top-Up"),
                ("line_items[0][price_data][unit_amount]", &amount),
                ("line_items[0][quantity]", "1"),
                ("success_url", success_url),
                ("cancel_url", cancel_url),
                ("metadata[type]", "topup"),
            ])
            .send()
            .await
            .context("Stripe: create topup session request")?;

        parse_response(resp).await.context("Stripe: create topup session")
    }

    /// Retrieve a subscription by ID.
    pub async fn get_subscription(&self, subscription_id: &str) -> Result<Subscription> {
        let resp = self.http
            .get(format!("{BASE_URL}/subscriptions/{subscription_id}"))
            .bearer_auth(&self.secret_key)
            .send()
            .await
            .context("Stripe: get subscription request")?;

        parse_response(resp).await.context("Stripe: get subscription")
    }

    /// Cancel a subscription immediately.
    pub async fn cancel_subscription(&self, subscription_id: &str) -> Result<Subscription> {
        let resp = self.http
            .delete(format!("{BASE_URL}/subscriptions/{subscription_id}"))
            .bearer_auth(&self.secret_key)
            .send()
            .await
            .context("Stripe: cancel subscription request")?;

        parse_response(resp).await.context("Stripe: cancel subscription")
    }

    /// Construct a Stripe webhook event from the raw body and signature header.
    /// Returns the parsed event or an error if the signature is invalid.
    pub fn verify_webhook(
        &self,
        payload: &[u8],
        sig_header: &str,
        webhook_secret: &str,
    ) -> Result<WebhookEvent> {
        let parts: Vec<&str> = sig_header.split(',').collect();

        let timestamp = parts.iter()
            .find_map(|p| p.strip_prefix("t="))
            .context("Stripe webhook: missing timestamp")?;

        let expected_sig = parts.iter()
            .find_map(|p| p.strip_prefix("v1="))
            .context("Stripe webhook: missing v1 signature")?;

        // Compute HMAC-SHA256(webhook_secret, "{timestamp}.{payload}")
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let mut mac = Hmac::<Sha256>::new_from_slice(webhook_secret.as_bytes())
            .context("Stripe webhook: invalid HMAC key")?;
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(payload);

        let computed = hex::encode(mac.finalize().into_bytes());
        if !constant_time_eq(computed.as_bytes(), expected_sig.as_bytes()) {
            bail!("Stripe webhook: signature mismatch");
        }

        // Reject events with timestamps older than 5 minutes to prevent replay attacks.
        let ts: i64 = timestamp.parse().context("Stripe webhook: invalid timestamp")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        if (now - ts).abs() > 300 {
            bail!("Stripe webhook: timestamp too old (possible replay)");
        }

        serde_json::from_slice(payload).context("Stripe webhook: parse event body")
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

async fn parse_response<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("Stripe API error {status}: {body}");
    }
    resp.json().await.context("deserializing Stripe response")
}

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Customer {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct CheckoutSession {
    pub id:  String,
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Subscription {
    pub id:     String,
    pub status: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub current_period_start: Option<i64>,
    pub current_period_end:   Option<i64>,
    pub items: Option<SubscriptionItems>,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionItems {
    pub data: Vec<SubscriptionItem>,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionItem {
    pub id: String,
    pub price: Option<Price>,
}

#[derive(Debug, Deserialize)]
pub struct Price {
    pub id: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Raw webhook event envelope. The `data.object` is kept as raw JSON
/// so handlers can deserialize into the specific type they need.
#[derive(Debug, Deserialize)]
pub struct WebhookEvent {
    pub id:      String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data:    WebhookEventData,
}

#[derive(Debug, Deserialize)]
pub struct WebhookEventData {
    pub object: serde_json::Value,
}
