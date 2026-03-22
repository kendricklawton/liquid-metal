pub mod billing;
pub mod cert_manager;
pub mod certs;
pub mod envelope;
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
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

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
    /// OIDC client ID for the CLI device flow (separate from web's OIDC_WEB_CLIENT_ID).
    pub oidc_cli_client_id: String,
    pub oidc_device_auth_url: String,
    pub oidc_token_url: String,
    pub oidc_userinfo_url: String,
    pub oidc_revoke_url: Option<String>,
    pub features: common::Features,
    /// Vault client for secret storage (env vars, TLS certs, internal secrets).
    pub vault: Arc<common::vault::VaultClient>,
    /// Default resource quota applied to Metal VMs when no per-service override is set.
    /// Built from QUOTA_* env vars at startup.
    pub default_quota: common::events::ResourceQuota,
    /// Shared HTTP client for outbound requests (VictoriaLogs, etc.).
    pub http_client: reqwest::Client,
    /// VictoriaLogs base URL. Empty string = logs endpoint returns [].
    pub victorialogs_url: String,
    /// Stripe client. None = billing disabled (local dev).
    pub stripe: Option<stripe::StripeClient>,
    /// Stripe webhook signing secret.
    pub stripe_webhook_secret: Option<String>,
}

/// Rate limit configuration, built from env vars in `main.rs`.
pub struct RateLimitConfig {
    pub auth:      rate_limit::RateLimit,
    pub protected: rate_limit::RateLimit,
    pub bff:       rate_limit::UserRateLimit,
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Liquid Metal API", version = "0.1.0", description = "PaaS API for hardware-isolated compute"),
    paths(
        // Health
        routes::health,
        // Auth
        routes::cli_config,
        routes::provision_user,
        routes::cli_provision,
        // Admin
        routes::create_invites,
        // Users
        routes::get_me,
        // Workspaces
        routes::list_workspaces,
        routes::delete_workspace,
        // Projects
        routes::list_projects,
        routes::create_project,
        // Services
        routes::list_services,
        routes::get_service_logs,
        routes::stop_service,
        routes::delete_service,
        routes::get_env_vars,
        routes::set_env_vars,
        routes::unset_env_vars,
        // routes::scale_service removed — all services are serverless
        routes::list_domains,
        routes::add_domain,
        routes::verify_domain,
        routes::remove_domain,
        routes::list_deploys,
        routes::rollback_service,
        routes::restart_service,
        // Deployments
        routes::get_upload_url,
        routes::deploy_service,
        // Billing
        billing::get_balance,
        billing::get_usage,
        billing::get_ledger,
        billing::create_topup,
        billing::list_invoices,
        // Webhooks
        billing::stripe_webhook,
    ),
    components(schemas(
        common::contract::HealthResponse,
        common::contract::ProvisionRequest,
        common::contract::ProvisionResponse,
        common::contract::UserResponse,
        common::contract::WorkspaceResponse,
        common::contract::ProjectResponse,
        common::contract::CreateProjectRequest,
        common::contract::CreateProjectResponse,
        common::contract::ServiceResponse,
        common::contract::LogLineResponse,
        common::contract::UploadUrlRequest,
        common::contract::UploadUrlResponse,
        common::contract::DeployRequest,
        common::contract::DeployedService,
        common::contract::DeployResponse,
        common::contract::DeleteServiceResponse,
        common::contract::EnvVarsResponse,
        common::contract::SetEnvVarsRequest,
        common::contract::UnsetEnvVarsRequest,
        common::contract::DomainResponse,
        common::contract::AddDomainRequest,
        common::contract::VerifyDomainResponse,
        common::contract::DeploymentHistoryEntry,
        common::contract::DeploymentHistoryResponse,
        common::contract::RollbackRequest,
        common::contract::CreateInvitesRequest,
        common::contract::CreateInvitesResponse,
        common::contract::TopupRequest,
        common::contract::CheckoutResponse,
        common::contract::BalanceResponse,
        common::contract::UsageResponse,
        common::contract::LiquidUsage,
        common::contract::LedgerResponse,
        common::contract::LedgerEntry,
        common::contract::InvoiceListResponse,
        common::contract::InvoiceEntry,
    )),
    security(),
    modifiers(&SecurityAddon),
    tags(
        (name = "Health", description = "Health check endpoints"),
        (name = "Auth", description = "Authentication and user provisioning"),
        (name = "Admin", description = "Administrative operations"),
        (name = "Users", description = "User profile"),
        (name = "Workspaces", description = "Workspace management"),
        (name = "Projects", description = "Project management"),
        (name = "Services", description = "Service lifecycle"),
        (name = "Deployments", description = "Artifact upload and deploy"),
        (name = "Billing", description = "Credits, usage, and Stripe checkout"),
        (name = "Webhooks", description = "External webhook handlers"),
    ),
)]
pub struct ApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            utoipa::openapi::security::SecurityScheme::ApiKey(
                utoipa::openapi::security::ApiKey::Header(
                    utoipa::openapi::security::ApiKeyValue::new("X-Api-Key"),
                ),
            ),
        );
        components.add_security_scheme(
            "internal_secret",
            utoipa::openapi::security::SecurityScheme::ApiKey(
                utoipa::openapi::security::ApiKey::Header(
                    utoipa::openapi::security::ApiKeyValue::new("X-Internal-Secret"),
                ),
            ),
        );
    }
}

/// Assemble the full Axum application router.
pub fn build_router(state: Arc<AppState>, rl: RateLimitConfig) -> Router {
    // ─── PUBLIC ROUTES ────────────────────────────────────────────────────────
    let public = Router::new()
        .route("/healthz",         get(routes::health))
        .route("/auth/cli/config", get(routes::cli_config))
        // ACME HTTP-01 challenge responder — must be public (no auth).
        // HAProxy routes /.well-known/acme-challenge/* on :80 to this API.
        .route("/.well-known/acme-challenge/{token}", get(routes::acme_challenge))
        .with_state(state.clone());

    // ─── WEBHOOKS (public, signature-verified) ─────────────────────────────
    let webhooks = Router::new()
        .route("/webhooks/stripe", post(billing::stripe_webhook))
        .with_state(state.clone());

    // ─── INTERNAL ROUTES (X-Internal-Secret) ─────────────────────────────────
    // BFF calls carry X-On-Behalf-Of → per-user bucket (120 RPM default).
    // Direct calls (no header) → per-IP bucket (10 RPM, brute force protection).
    let bff_rl = rl.bff.clone();
    let auth_rl = rl.auth.clone();
    let internal = Router::new()
        .route("/auth/provision", post(routes::provision_user))
        .route("/admin/invites",  post(routes::create_invites))
        .route_layer(middleware::from_fn(move |req, next| {
            rate_limit::rate_limit_by_user_or_ip_middleware(bff_rl.clone(), auth_rl.clone(), req, next)
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
        .route("/projects/{id}/env",       get(routes::get_project_env_vars).post(routes::set_project_env_vars))
        .route("/projects/{id}/env/unset", post(routes::unset_project_env_vars))
        .route("/services",               get(routes::list_services))
        .route("/services/{id}/logs",      get(routes::get_service_logs))
        .route("/services/{id}/stop",      post(routes::stop_service))
        .route("/services/{id}/delete",    post(routes::delete_service))
        .route("/services/{id}/env",       get(routes::get_env_vars).post(routes::set_env_vars))
        .route("/services/{id}/env/unset", post(routes::unset_env_vars))
        // scale route removed — all services are serverless
        .route("/services/{id}/domains",   get(routes::list_domains).post(routes::add_domain))
        .route("/services/{id}/domains/{domain}/verify", post(routes::verify_domain))
        .route("/services/{id}/domains/{domain}/remove", post(routes::remove_domain))
        .route("/services/{id}/deploys",   get(routes::list_deploys))
        .route("/services/{id}/rollback",  post(routes::rollback_service))
        .route("/services/{id}/restart",   post(routes::restart_service))
        .route("/deployments/upload-url",  post(routes::get_upload_url))
        .route("/deployments",             post(routes::deploy_service))
        .route("/deployments/{id}/stream", get(routes::stream_deploy))
        .route("/billing/balance",   get(billing::get_balance))
        .route("/billing/usage",     get(billing::get_usage))
        .route("/billing/ledger",    get(billing::get_ledger))
        .route("/billing/topup",     post(billing::create_topup))
        .route("/billing/invoices",  get(billing::list_invoices))
        .route("/api-keys",          get(routes::list_api_keys).post(routes::create_api_key))
        .route("/api-keys/{id}",     delete(routes::delete_api_key))
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
        .merge(SwaggerUi::new("/docs").url("/docs/openapi.json", ApiDoc::openapi()))
        .layer(middleware::from_fn(metrics_middleware))
        .layer(TraceLayer::new_for_http())
}

async fn metrics_middleware(
    matched_path: Option<axum::extract::MatchedPath>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let method = req.method().to_string();
    let path = matched_path
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unknown".into());
    let start = std::time::Instant::now();

    let response = next.run(req).await;

    let elapsed = start.elapsed();
    let status_code = response.status().as_u16();
    let status = status_code.to_string();
    metrics::counter!("http_requests_total",
        "method" => method.clone(), "path" => path.clone(), "status" => status)
        .increment(1);
    metrics::histogram!("http_request_duration_seconds",
        "method" => method.clone(), "path" => path.clone())
        .record(elapsed.as_secs_f64());

    // Structured request log — 12-factor: treat logs as event streams.
    // Skip noisy health checks and metrics scrapes.
    if path != "/healthz" && path != "/metrics" {
        tracing::info!(
            http.method = %method,
            http.route = %path,
            http.status = status_code,
            duration_ms = elapsed.as_millis() as u64,
            "request"
        );
    }

    response
}
