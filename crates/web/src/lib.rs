pub mod api_client;
pub mod error;
pub mod routes;
pub mod session;

use std::sync::Arc;

use axum::{Router, routing::get};
use tower_http::{services::ServeDir, trace::TraceLayer};

pub struct AppState {
    /// Internal API base URL (e.g. `http://localhost:7070`).
    pub api_url: String,
    /// Internal secret for API calls (first value from INTERNAL_SECRET).
    pub internal_secret: String,
    /// Shared HTTP client — reuses connections via hyper's pool.
    pub http_client: reqwest::Client,
    /// OIDC client ID for browser auth.
    pub oidc_client_id: String,
    /// OIDC authorization endpoint (browser redirect flow, not device flow).
    pub oidc_auth_url: String,
    /// OIDC token endpoint.
    pub oidc_token_url: String,
    /// OIDC userinfo endpoint.
    pub oidc_userinfo_url: String,
    /// OIDC revocation endpoint (optional — enables logout token revocation).
    pub oidc_revoke_url: Option<String>,
    /// Redirect URI for OIDC callback (e.g. `http://localhost:3000/auth/callback`).
    pub oidc_redirect_uri: String,
    /// AES-GCM key for encrypting session cookies (32 bytes).
    pub cookie_key: axum_extra::extract::cookie::Key,
    /// Feature flags loaded from env.
    pub features: common::Features,
    /// Dev bypass — skip auth and inject a mock session (set DISABLE_AUTH=1).
    pub disable_auth: bool,
}

/// Assemble the full Axum application router.
pub fn build_router(state: Arc<AppState>) -> Router {
    let public = Router::new()
        .route("/", get(routes::public::splash))
        .route("/plans", get(routes::public::plans))
        .route("/about", get(routes::public::about))
        .route("/docs", get(routes::public::docs))
        .route("/help", get(routes::public::help))
        .route("/templates", get(routes::public::templates))
        .route("/changelog", get(routes::public::changelog))
        .route("/status", get(routes::public::status));

    // compile-time absolute path; override with WEB_STATIC_DIR for Docker/prod.
    let static_dir = std::env::var("WEB_STATIC_DIR")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/static").to_string());
    let statics = Router::new()
        .nest_service("/static", ServeDir::new(static_dir));

    public
        .merge(statics)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
