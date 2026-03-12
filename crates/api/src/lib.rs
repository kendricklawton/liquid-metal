pub mod billing;
pub mod migrations;
pub mod nats;
pub mod outbox;
pub mod quota;
pub mod rate_limit;
pub mod routes;
pub mod storage;
pub mod stripe;

use axum::{
    Router, middleware,
    routing::{delete, get, post},
};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub db: deadpool_postgres::Pool,
    pub nats: async_nats::jetstream::Context,
    pub nats_client: async_nats::Client,
    pub s3: aws_sdk_s3::Client,
    pub bucket: String,
    /// Comma-separated list of valid secrets. During rotation, set both old and
    /// new values: `INTERNAL_SECRET=new-secret,old-secret`. Remove the old value
    /// after all callers have been updated.
    pub internal_secrets: Vec<String>,
    pub oidc_client_id: String,
    pub oidc_device_auth_url: String,
    pub oidc_token_url: String,
    pub oidc_userinfo_url: String,
    pub oidc_revoke_url: Option<String>,
    pub features: common::Features,
    /// Total allocatable Metal RAM across all nodes (MB). Set via METAL_CAPACITY_MB.
    /// Used by the hobby tier capacity gate. 0 = gate disabled (no Metal nodes).
    pub metal_capacity_mb: i64,
    /// Shared HTTP client for outbound requests (VictoriaLogs, etc.).
    pub http_client: reqwest::Client,
    /// VictoriaLogs base URL. Empty string = logs endpoint returns [].
    pub victorialogs_url: String,
    /// Stripe client. None = billing disabled (local dev).
    pub stripe: Option<stripe::StripeClient>,
    /// Stripe webhook signing secret.
    pub stripe_webhook_secret: Option<String>,
    /// Stripe Price IDs for Pro and Team subscriptions.
    pub stripe_price_pro:  Option<String>,
    pub stripe_price_team: Option<String>,
}

/// Rate limit configuration, built from env vars in `main.rs`.
pub struct RateLimitConfig {
    pub auth:      rate_limit::RateLimit,
    pub protected: rate_limit::RateLimit,
}

/// Assemble the full Axum application router.
pub fn build_router(state: Arc<AppState>, rl: RateLimitConfig) -> Router {
    // ─── PUBLIC ROUTES ────────────────────────────────────────────────────────
    let public = Router::new()
        .route("/healthz",         get(routes::health))
        .route("/auth/cli/config", get(routes::cli_config))
        .with_state(state.clone());

    // ─── WEBHOOKS (public, signature-verified) ─────────────────────────────
    let webhooks = Router::new()
        .route("/webhooks/stripe", post(billing::stripe_webhook))
        .with_state(state.clone());

    // ─── INTERNAL ROUTES (X-Internal-Secret) ─────────────────────────────────
    // Rate-limited: these are unauthenticated (secret-gated) and brute-forceable.
    let auth_rl = rl.auth.clone();
    let internal = Router::new()
        .route("/auth/provision", post(routes::provision_user))
        .route("/admin/invites",  post(routes::create_invites))
        .route_layer(middleware::from_fn(move |req, next| {
            rate_limit::rate_limit_middleware(auth_rl.clone(), req, next)
        }))
        .with_state(state.clone());

    // ─── CLI AUTH (device flow — no token required) ───────────────────────────
    // Rate-limited: invite code brute-force and OIDC callback floods.
    let auth_rl = rl.auth.clone();
    let cli_auth = Router::new()
        .route("/auth/cli/provision", post(routes::cli_provision))
        .route_layer(middleware::from_fn(move |req, next| {
            rate_limit::rate_limit_middleware(auth_rl.clone(), req, next)
        }))
        .with_state(state.clone());

    // ─── PROTECTED REST (X-Api-Key validated by auth_middleware) ─────────────
    let api_rl = rl.protected.clone();
    let protected = Router::new()
        .route("/users/me",               get(routes::get_me))
        .route("/workspaces",             get(routes::list_workspaces))
        .route("/workspaces/{id}",        delete(routes::delete_workspace))
        .route("/projects",               get(routes::list_projects).post(routes::create_project))
        .route("/services",               get(routes::list_services))
        .route("/services/{id}/logs",      get(routes::get_service_logs))
        .route("/services/{id}/stop",      post(routes::stop_service))
        .route("/services/{id}/restart",   post(routes::restart_service))
        .route("/deployments/upload-url", post(routes::get_upload_url))
        .route("/deployments",            post(routes::deploy_service))
        .route("/billing/balance",   get(billing::get_balance))
        .route("/billing/usage",     get(billing::get_usage))
        .route("/billing/ledger",    get(billing::get_ledger))
        .route("/billing/topup",     post(billing::create_topup))
        .route("/billing/subscribe", post(billing::create_subscription))
        .route_layer(middleware::from_fn(move |req, next| {
            rate_limit::rate_limit_middleware(api_rl.clone(), req, next)
        }))
        .route_layer(middleware::from_fn_with_state(state.clone(), routes::auth_middleware))
        .with_state(state.clone());

    public
        .merge(webhooks)
        .merge(internal)
        .merge(cli_auth)
        .merge(protected)
        .layer(TraceLayer::new_for_http())
}
