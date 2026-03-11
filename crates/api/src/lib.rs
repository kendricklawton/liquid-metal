pub mod migrations;
pub mod nats;
pub mod quota;
pub mod routes;
pub mod storage;

use axum::{
    Router, middleware,
    routing::{get, post},
};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub db: deadpool_postgres::Pool,
    pub nats: async_nats::jetstream::Context,
    pub nats_client: async_nats::Client,
    pub s3: aws_sdk_s3::Client,
    pub bucket: String,
    pub internal_secret: String,
    pub oidc_client_id: String,
    pub oidc_device_auth_url: String,
    pub oidc_token_url: String,
    pub oidc_userinfo_url: String,
    pub oidc_revoke_url: Option<String>,
    pub features: common::Features,
}

/// Assemble the full Axum application router.
pub fn build_router(state: Arc<AppState>) -> Router {
    // ─── PUBLIC ROUTES ────────────────────────────────────────────────────────
    let public = Router::new()
        .route("/healthz",         get(routes::health))
        .route("/auth/cli/config", get(routes::cli_config))
        .with_state(state.clone());

    // ─── INTERNAL ROUTES (X-Internal-Secret) ─────────────────────────────────
    let internal = Router::new()
        .route("/auth/provision", post(routes::provision_user))
        .route("/admin/invites",  post(routes::create_invites))
        .with_state(state.clone());

    // ─── CLI AUTH (device flow — no token required) ───────────────────────────
    let cli_auth = Router::new()
        .route("/auth/cli/provision", post(routes::cli_provision))
        .with_state(state.clone());

    // ─── PROTECTED REST (X-Api-Key validated by auth_middleware) ─────────────
    let protected = Router::new()
        .route("/users/me",               get(routes::get_me))
        .route("/workspaces",             get(routes::list_workspaces))
        .route("/projects",               get(routes::list_projects).post(routes::create_project))
        .route("/services",               get(routes::list_services))
        .route("/services/{id}/logs",      get(routes::get_service_logs))
        .route("/services/{id}/stop",      post(routes::stop_service))
        .route("/services/{id}/restart",   post(routes::restart_service))
        .route("/deployments/upload-url", post(routes::get_upload_url))
        .route("/deployments",            post(routes::deploy_service))
        .route_layer(middleware::from_fn_with_state(state.clone(), routes::auth_middleware))
        .with_state(state.clone());

    public
        .merge(internal)
        .merge(cli_auth)
        .merge(protected)
        .layer(TraceLayer::new_for_http())
}
