//! Integration tests for the liquid-metal API.
//!
//! These tests call the full Axum router in-process via `tower::ServiceExt::oneshot`
//! — no network socket needed, but Postgres + NATS must be reachable.
//!
//! Prerequisites:
//!   task up   # starts docker-compose (Postgres + NATS)
//!
//! Run:
//!   DATABASE_URL=postgres://postgres:postgres@localhost:5432/liquidmetal \
//!   INTERNAL_SECRET=test-secret \
//!   cargo test -p api -- --include-ignored

use api::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

// ── test helpers ─────────────────────────────────────────────────────────────

/// Build a full AppState from environment variables.
/// Returns `None` if `DATABASE_URL` is unset — tests that call this should
/// call `.expect("set DATABASE_URL to run integration tests")`.
async fn try_build_state() -> Option<Arc<AppState>> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let nats_url = std::env::var("NATS_URL")
        .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let bucket = std::env::var("OBJECT_STORAGE_BUCKET")
        .unwrap_or_else(|_| "test-bucket".to_string());
    let internal_secret = std::env::var("INTERNAL_SECRET")
        .unwrap_or_else(|_| "test-secret".to_string());

    let pg_cfg: tokio_postgres::Config = db_url.parse().ok()?;
    let mgr  = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
    let pool = deadpool_postgres::Pool::builder(mgr).max_size(4).build().ok()?;

    api::migrations::run(&pool).await.ok()?;

    let nc = async_nats::connect(&nats_url).await.ok()?;
    let js = async_nats::jetstream::new(nc.clone());
    api::nats::ensure_stream(&js).await.ok()?;

    let s3 = api::storage::build_client().await;

    Some(Arc::new(AppState {
        db: pool,
        nats: js,
        nats_client: nc,
        s3,
        bucket,
        internal_secret,
        oidc_client_id:       std::env::var("OIDC_CLIENT_ID").unwrap_or_default(),
        oidc_device_auth_url: std::env::var("OIDC_DEVICE_AUTH_URL").unwrap_or_default(),
        oidc_token_url:       std::env::var("OIDC_TOKEN_URL").unwrap_or_default(),
        oidc_userinfo_url:    std::env::var("OIDC_USERINFO_URL").unwrap_or_default(),
        oidc_revoke_url:      std::env::var("OIDC_REVOKE_URL").ok(),
        features:             common::Features::from_env(),
    }))
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

// ── /healthz ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn healthz_returns_ok() {
    let state = try_build_state().await.expect("set DATABASE_URL to run integration tests");
    let app = build_router(state);

    let resp = app
        .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp.into_body()).await;
    assert_eq!(v["status"], "ok");
    assert_eq!(v["db"],   "ok");
    assert_eq!(v["nats"], "ok");
}

// ── /auth/provision ───────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn provision_rejects_wrong_secret() {
    let state = try_build_state().await.expect("set DATABASE_URL to run integration tests");
    let app = build_router(state);

    let body = json!({ "email": "wrong@example.com", "first_name": "A", "last_name": "B" });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/provision")
                .header("content-type", "application/json")
                .header("x-internal-secret", "definitely-wrong")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn provision_rejects_missing_secret() {
    let state = try_build_state().await.expect("set DATABASE_URL to run integration tests");
    let app = build_router(state);

    let body = json!({ "email": "test@example.com", "first_name": "A", "last_name": "B" });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/provision")
                .header("content-type", "application/json")
                // no x-internal-secret header
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn provision_creates_user_and_is_idempotent() {
    let state = try_build_state().await.expect("set DATABASE_URL to run integration tests");
    let secret = state.internal_secret.clone();
    let app   = build_router(state);

    // Unique email per test run to avoid cross-test collisions
    let email = format!("integ-{}@example.com", uuid::Uuid::now_v7());
    let body  = json!({ "email": email, "first_name": "Alice", "last_name": "Smith" });

    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/auth/provision")
            .header("content-type", "application/json")
            .header("x-internal-secret", &secret)
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    // ── First call: new user ──────────────────────────────────────────────────
    let resp1 = app.clone().oneshot(make_request()).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK, "first provision should succeed");
    let v1 = body_json(resp1.into_body()).await;

    let id = v1["id"].as_str().expect("id should be present");
    assert!(!id.is_empty());
    assert_eq!(v1["name"], "Alice Smith");
    assert!(
        v1["slug"].as_str().unwrap().ends_with("-workspace"),
        "slug should end with -workspace"
    );
    assert_eq!(v1["tier"], "hobby");

    // ── Second call: idempotent — same user_id returned ───────────────────────
    let resp2 = app.oneshot(make_request()).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK, "second provision should also succeed");
    let v2 = body_json(resp2.into_body()).await;

    assert_eq!(v1["id"], v2["id"], "provision must be idempotent: same user_id on repeat call");
    assert_eq!(v1["slug"], v2["slug"], "workspace slug must be stable");
}

// ── /services (GET — requires auth) ──────────────────────────────────────────

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn list_services_rejects_missing_key() {
    let state = try_build_state().await.expect("set DATABASE_URL to run integration tests");
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/services")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // No X-Api-Key → auth middleware returns 401
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
