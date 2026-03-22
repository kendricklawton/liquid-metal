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
                ("invoice_creation[enabled]", "true"),
            ])
            .send()
            .await
            .context("Stripe: create topup session request")?;

        parse_response(resp).await.context("Stripe: create topup session")
    }

    /// Create a Stripe Invoice (draft) for a customer.
    /// Used for Metal/Liquid usage summaries that debit from prepaid balance.
    /// `collection_method = "send_invoice"` because payment is already settled
    /// via the credit balance — this invoice is a receipt/record, not a charge.
    pub async fn create_invoice(
        &self,
        customer_id: &str,
        description: &str,
        days_until_due: u32,
        metadata: &[(&str, &str)],
    ) -> Result<Invoice> {
        let days = days_until_due.to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("customer", customer_id),
            ("collection_method", "send_invoice"),
            ("days_until_due", &days),
            ("description", description),
            ("auto_advance", "true"),
        ];
        for (k, v) in metadata {
            params.push((k, v));
        }

        let resp = self.http
            .post(format!("{BASE_URL}/invoices"))
            .bearer_auth(&self.secret_key)
            .form(&params)
            .send()
            .await
            .context("Stripe: create invoice request")?;

        parse_response(resp).await.context("Stripe: create invoice")
    }

    /// Add a line item to a draft invoice.
    pub async fn add_invoice_item(
        &self,
        customer_id: &str,
        invoice_id: &str,
        description: &str,
        amount_cents: i64,
    ) -> Result<InvoiceItem> {
        let amount = amount_cents.to_string();
        let resp = self.http
            .post(format!("{BASE_URL}/invoiceitems"))
            .bearer_auth(&self.secret_key)
            .form(&[
                ("customer", customer_id),
                ("invoice", invoice_id),
                ("description", description),
                ("amount", &amount),
                ("currency", "usd"),
            ])
            .send()
            .await
            .context("Stripe: add invoice item request")?;

        parse_response(resp).await.context("Stripe: add invoice item")
    }

    /// Finalize a draft invoice (makes it immutable, generates PDF).
    pub async fn finalize_invoice(&self, invoice_id: &str) -> Result<Invoice> {
        let resp = self.http
            .post(format!("{BASE_URL}/invoices/{invoice_id}/finalize"))
            .bearer_auth(&self.secret_key)
            .send()
            .await
            .context("Stripe: finalize invoice request")?;

        parse_response(resp).await.context("Stripe: finalize invoice")
    }

    /// Mark a finalized invoice as paid (for invoices settled via prepaid balance).
    pub async fn mark_invoice_paid(&self, invoice_id: &str) -> Result<Invoice> {
        let resp = self.http
            .post(format!("{BASE_URL}/invoices/{invoice_id}/pay"))
            .bearer_auth(&self.secret_key)
            .form(&[("paid_out_of_band", "true")])
            .send()
            .await
            .context("Stripe: mark invoice paid request")?;

        parse_response(resp).await.context("Stripe: mark invoice paid")
    }

    /// List invoices for a customer (most recent first).
    pub async fn list_invoices(
        &self,
        customer_id: &str,
        limit: u32,
    ) -> Result<InvoiceList> {
        let limit_str = limit.to_string();
        let resp = self.http
            .get(format!("{BASE_URL}/invoices"))
            .bearer_auth(&self.secret_key)
            .query(&[
                ("customer", customer_id),
                ("limit", &limit_str),
            ])
            .send()
            .await
            .context("Stripe: list invoices request")?;

        parse_response(resp).await.context("Stripe: list invoices")
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
pub struct Invoice {
    pub id:          String,
    pub number:      Option<String>,
    pub status:      Option<String>,
    pub hosted_invoice_url: Option<String>,
    pub invoice_pdf: Option<String>,
    pub amount_due:  Option<i64>,
    pub amount_paid: Option<i64>,
    pub created:     Option<i64>,
    pub period_start: Option<i64>,
    pub period_end:  Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct InvoiceItem {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct InvoiceList {
    pub data:     Vec<Invoice>,
    pub has_more: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> StripeClient {
        StripeClient::new("sk_test_fake".to_string(), reqwest::Client::new())
    }

    // ── Webhook signature verification ──────────────────────────────────

    #[test]
    fn verify_webhook_valid_signature() {
        let client = test_client();
        let secret = "whsec_test_secret";
        let payload = br#"{"id":"evt_1","type":"checkout.session.completed","data":{"object":{}}}"#;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Compute valid HMAC-SHA256
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(payload);
        let sig = hex::encode(mac.finalize().into_bytes());

        let header = format!("t={},v1={}", timestamp, sig);
        let event = client.verify_webhook(payload, &header, secret).unwrap();
        assert_eq!(event.id, "evt_1");
        assert_eq!(event.event_type, "checkout.session.completed");
    }

    #[test]
    fn verify_webhook_rejects_bad_signature() {
        let client = test_client();
        let payload = br#"{"id":"evt_1","type":"test","data":{"object":{}}}"#;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let header = format!("t={},v1=deadbeef0000000000000000000000000000000000000000000000000000dead", timestamp);
        let result = client.verify_webhook(payload, &header, "whsec_real");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("signature mismatch"));
    }

    #[test]
    fn verify_webhook_rejects_missing_timestamp() {
        let client = test_client();
        let result = client.verify_webhook(b"{}", "v1=abc123", "whsec_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing timestamp"));
    }

    #[test]
    fn verify_webhook_rejects_missing_v1() {
        let client = test_client();
        let result = client.verify_webhook(b"{}", "t=1234567890", "whsec_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing v1 signature"));
    }

    #[test]
    fn verify_webhook_rejects_stale_timestamp() {
        let client = test_client();
        let secret = "whsec_test";
        let payload = br#"{"id":"evt_1","type":"test","data":{"object":{}}}"#;

        // 10 minutes ago — outside the 5-minute tolerance
        let stale_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() - 600;

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(stale_ts.to_string().as_bytes());
        mac.update(b".");
        mac.update(payload);
        let sig = hex::encode(mac.finalize().into_bytes());

        let header = format!("t={},v1={}", stale_ts, sig);
        let result = client.verify_webhook(payload, &header, secret);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timestamp too old"));
    }

    // ── constant_time_eq ────────────────────────────────────────────────

    #[test]
    fn constant_time_eq_same_strings() {
        assert!(constant_time_eq(b"hello", b"hello"));
    }

    #[test]
    fn constant_time_eq_different_strings() {
        assert!(!constant_time_eq(b"hello", b"world"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"short", b"longer string"));
    }

    #[test]
    fn constant_time_eq_empty() {
        assert!(constant_time_eq(b"", b""));
    }

    // ── Response type deserialization ────────────────────────────────────

    #[test]
    fn deserialize_invoice() {
        let json = r#"{
            "id": "in_1234",
            "number": "LM-0001",
            "status": "paid",
            "hosted_invoice_url": "https://invoice.stripe.com/i/1234",
            "invoice_pdf": "https://pay.stripe.com/invoice/1234/pdf",
            "amount_due": 2000,
            "amount_paid": 2000,
            "created": 1700000000,
            "period_start": 1697400000,
            "period_end": 1700000000
        }"#;
        let inv: Invoice = serde_json::from_str(json).unwrap();
        assert_eq!(inv.id, "in_1234");
        assert_eq!(inv.number.as_deref(), Some("LM-0001"));
        assert_eq!(inv.status.as_deref(), Some("paid"));
        assert_eq!(inv.amount_due, Some(2000));
        assert_eq!(inv.invoice_pdf.as_deref(), Some("https://pay.stripe.com/invoice/1234/pdf"));
    }

    #[test]
    fn deserialize_invoice_minimal() {
        // Stripe may omit optional fields on draft invoices
        let json = r#"{"id": "in_draft_1"}"#;
        let inv: Invoice = serde_json::from_str(json).unwrap();
        assert_eq!(inv.id, "in_draft_1");
        assert!(inv.number.is_none());
        assert!(inv.status.is_none());
        assert!(inv.invoice_pdf.is_none());
    }

    #[test]
    fn deserialize_invoice_list() {
        let json = r#"{
            "data": [
                {"id": "in_1", "number": "LM-0001", "status": "paid"},
                {"id": "in_2", "status": "open"}
            ],
            "has_more": false
        }"#;
        let list: InvoiceList = serde_json::from_str(json).unwrap();
        assert_eq!(list.data.len(), 2);
        assert!(!list.has_more);
        assert_eq!(list.data[0].id, "in_1");
        assert_eq!(list.data[1].id, "in_2");
    }

    #[test]
    fn deserialize_invoice_item() {
        let json = r#"{"id": "ii_123"}"#;
        let item: InvoiceItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.id, "ii_123");
    }

    #[test]
    fn deserialize_checkout_session() {
        let json = r#"{"id": "cs_1234", "url": "https://checkout.stripe.com/pay/cs_1234"}"#;
        let s: CheckoutSession = serde_json::from_str(json).unwrap();
        assert_eq!(s.id, "cs_1234");
        assert_eq!(s.url.as_deref(), Some("https://checkout.stripe.com/pay/cs_1234"));
    }

    #[test]
    fn deserialize_webhook_event() {
        let json = r#"{
            "id": "evt_1",
            "type": "checkout.session.completed",
            "data": {"object": {"id": "cs_1", "customer": "cus_1", "amount_total": 5000}}
        }"#;
        let ev: WebhookEvent = serde_json::from_str(json).unwrap();
        assert_eq!(ev.id, "evt_1");
        assert_eq!(ev.event_type, "checkout.session.completed");
        assert_eq!(ev.data.object["customer"], "cus_1");
        assert_eq!(ev.data.object["amount_total"], 5000);
    }
}
