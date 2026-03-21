//! Shared test harness for API integration tests.
//!
//! Provides `TestHarness` — owns AppState + Router with helper methods
//! for building requests and provisioning test users.

use api::{AppState, RateLimitConfig, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

// ── TestHarness ─────────────────────────────────────────────────────────────

pub struct TestHarness {
    app: Router,
    internal_secret: String,
}

impl TestHarness {
    /// Build a harness from environment variables.
    /// Returns `None` if `DATABASE_URL` is unset (caller should `.expect()`).
    pub async fn new() -> Option<Self> {
        let db_url = std::env::var("DATABASE_URL").ok()?;
        let nats_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let bucket =
            std::env::var("OBJECT_STORAGE_BUCKET").unwrap_or_else(|_| "test-bucket".to_string());
        let internal_secret =
            std::env::var("INTERNAL_SECRET").unwrap_or_else(|_| "test-secret".to_string());

        let pg_cfg: tokio_postgres::Config = db_url.parse().ok()?;
        let pool = if let Some(tls) = common::config::pg_tls().ok()? {
            let mgr = deadpool_postgres::Manager::new(pg_cfg, tls);
            deadpool_postgres::Pool::builder(mgr)
                .max_size(4)
                .build()
                .ok()?
        } else {
            let mgr = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
            deadpool_postgres::Pool::builder(mgr)
                .max_size(4)
                .build()
                .ok()?
        };

        api::migrations::run_with_url(&db_url).await.ok()?;

        let nc = common::config::nats_connect(&nats_url).await.ok()?;
        let js = async_nats::jetstream::new(nc.clone());
        api::nats::ensure_stream(&js).await.ok()?;

        let s3 = api::storage::build_client().ok()?;

        let state = Arc::new(AppState {
            db: pool,
            nats: js,
            nats_client: nc,
            s3,
            bucket,
            internal_secrets: vec![internal_secret.clone()],
            oidc_cli_client_id: std::env::var("OIDC_CLI_CLIENT_ID").unwrap_or_default(),
            oidc_device_auth_url: String::new(),
            oidc_token_url: String::new(),
            oidc_userinfo_url: String::new(),
            oidc_revoke_url: None,
            features: common::Features::from_env(),
            vault: Arc::new(common::vault::VaultClient::from_env()
                .unwrap_or_else(|_| common::vault::VaultClient::new("http://localhost:8200", "dev-root-token"))),
            default_quota: Default::default(),
            http_client: reqwest::Client::new(),
            victorialogs_url: String::new(),
            stripe: None,
            stripe_webhook_secret: None,
            stripe_price_pro: None,
            stripe_price_team: None,
        });

        let rate_limits = RateLimitConfig {
            auth: api::rate_limit::RateLimit::per_minute(1000),
            protected: api::rate_limit::RateLimit::per_minute(1000),
        };

        let app = build_router(state.clone(), rate_limits);

        Some(Self { app, internal_secret })
    }

    // ── Request Builders ────────────────────────────────────────────────────

    /// Build an unauthenticated GET request.
    pub fn get(&self, uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    /// Build an unauthenticated POST with JSON body.
    pub fn post_json(&self, uri: &str, body: &Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Build a POST with X-Internal-Secret header (no user context).
    pub fn internal_post(&self, uri: &str, body: &Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("x-internal-secret", &self.internal_secret)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Build a POST with internal service auth (X-Internal-Secret + X-On-Behalf-Of).
    pub fn internal_authed_post(&self, uri: &str, body: &Value, user_id: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("x-internal-secret", &self.internal_secret)
            .header("x-on-behalf-of", user_id)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Build an authenticated GET (X-Api-Key with lm_* token).
    pub fn authed_get(&self, uri: &str, api_key: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header("x-api-key", api_key)
            .body(Body::empty())
            .unwrap()
    }

    /// Build an authenticated POST with JSON body (X-Api-Key with lm_* token).
    pub fn authed_post(&self, uri: &str, body: &Value, api_key: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("x-api-key", api_key)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Build an authenticated POST with no body (X-Api-Key with lm_* token).
    pub fn authed_post_empty(&self, uri: &str, api_key: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("x-api-key", api_key)
            .body(Body::empty())
            .unwrap()
    }

    // ── Send Helpers ────────────────────────────────────────────────────────

    /// Send a request through the router and return (status, parsed JSON body).
    pub async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(json!(null));
        (status, body)
    }

    /// Send and assert the expected status code. Returns the JSON body.
    pub async fn send_ok(&self, req: Request<Body>) -> Value {
        let (status, body) = self.send(req).await;
        assert_eq!(status, StatusCode::OK, "expected 200, got {}: {:?}", status, body);
        body
    }

    /// Send and assert a specific status code. Returns the JSON body.
    pub async fn send_expect(&self, req: Request<Body>, expected: StatusCode) -> Value {
        let (status, body) = self.send(req).await;
        assert_eq!(status, expected, "expected {}, got {}: {:?}", expected, status, body);
        body
    }

    // ── High-Level Helpers ──────────────────────────────────────────────────

    /// Provision a test user with a unique email. Returns a TestUser with
    /// a real `lm_*` scoped API key for authenticated requests.
    pub async fn provision_user(&self) -> TestUser {
        let email = format!("test-{}@example.com", uuid::Uuid::now_v7());
        let body = json!({
            "email": email,
            "first_name": "Test",
            "last_name": "User"
        });
        let resp = self.send_ok(self.internal_post("/auth/provision", &body)).await;
        let user_id = resp["id"].as_str().unwrap().to_string();

        // Create an API key via internal service auth
        let key_body = json!({ "name": "test-key", "scopes": ["admin"] });
        let key_resp = self.send_expect(
            self.internal_authed_post("/api-keys", &key_body, &user_id),
            StatusCode::CREATED,
        ).await;

        TestUser {
            id: user_id,
            workspace_id: resp["workspace_id"].as_str().unwrap().to_string(),
            email,
            api_key: key_resp["token"].as_str().unwrap().to_string(),
        }
    }

    /// Create a project under a workspace. Returns the project_id.
    pub async fn create_project(&self, api_key: &str, workspace_id: &str, name: &str) -> String {
        let slug = name.to_lowercase().replace(' ', "-");
        let body = json!({
            "workspace_id": workspace_id,
            "name": name,
            "slug": slug,
        });
        let resp = self.send_ok(self.authed_post("/projects", &body, api_key)).await;
        resp["project"]["id"].as_str().unwrap().to_string()
    }

    /// Deploy a minimal Liquid service. Returns the service_id.
    pub async fn deploy_service(&self, user: &TestUser, name: &str) -> String {
        let project_id = self.create_project(user.api_key(), &user.workspace_id, name).await;
        let deploy_id = uuid::Uuid::now_v7().to_string();
        let url_resp = self.send_ok(self.authed_post(
            "/deployments/upload-url",
            &json!({ "engine": "liquid", "deploy_id": deploy_id, "project_id": project_id }),
            user.api_key(),
        )).await;

        let http = reqwest::Client::new();
        http.put(url_resp["upload_url"].as_str().unwrap())
            .body(b"\x00asm\x01\x00\x00\x00".to_vec())
            .send().await.expect("S3 upload should succeed");

        let deploy_resp = self.send_ok(self.authed_post(
            "/deployments",
            &json!({
                "name": name, "slug": name, "engine": "liquid",
                "project_id": project_id,
                "artifact_key": url_resp["artifact_key"].as_str().unwrap(),
                "sha256": "93a44bbb96c751218e4c00d479e4c14358122a389acca16205b1e4d0dc5f9476",
            }),
            user.api_key(),
        )).await;
        deploy_resp["service"]["id"].as_str().unwrap().to_string()
    }
}

// ── TestUser ────────────────────────────────────────────────────────────────

pub struct TestUser {
    /// User UUID (for X-On-Behalf-Of internal service calls).
    pub id: String,
    pub workspace_id: String,
    pub email: String,
    /// Scoped `lm_*` API key for authenticated requests.
    api_key: String,
}

impl TestUser {
    /// Return the `lm_*` scoped API key for authenticated requests.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }
}
