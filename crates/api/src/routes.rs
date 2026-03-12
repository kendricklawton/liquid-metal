use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::AppState;

/// Hobby tier capacity gate: reject Metal deploys when live nodes exceed this
/// fraction of total allocatable RAM. Expressed as a multiplied comparison to
/// avoid floating point: allocated_mb * 10 > capacity_mb * THRESHOLD_NUMERATOR.
const HOBBY_CAPACITY_THRESHOLD_NUMERATOR: i64 = 8; // 8/10 = 80%

/// Maximum time to wait for a DB connection from the pool before returning 503.
/// Prevents cascading failures under burst load — callers get a clear "try again"
/// instead of queuing indefinitely behind a saturated pool.
const DB_POOL_TIMEOUT: Duration = Duration::from_secs(5);

// ── Structured API error ─────────────────────────────────────────────────────

/// Structured error response returned by all API handlers.
///
/// Serializes to `{"error": "...", "message": "..."}` so clients can
/// programmatically distinguish error types and show human-readable context.
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    error: &'static str,
    message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, error: &'static str, message: impl Into<String>) -> Self {
        Self { status, error, message: message.into() }
    }

    pub(crate) fn bad_request(error: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error, message)
    }

    pub(crate) fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    pub(crate) fn unprocessable(error: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, error, message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
    }

    pub(crate) fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "bad_gateway", message)
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error":   self.error,
            "message": self.message,
        });
        (self.status, Json(body)).into_response()
    }
}

/// Verify the X-Internal-Secret header using constant-time comparison.
/// Accepts any of the provided secrets — supports zero-downtime rotation
/// by setting `INTERNAL_SECRET=new-secret,old-secret` during the rotation window.
/// Returns 401 if the header is missing/empty, 403 if the value doesn't match.
fn verify_internal_secret(headers: &HeaderMap, valid_secrets: &[String]) -> Result<(), ApiError> {
    let secret = headers
        .get("x-internal-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if secret.is_empty() {
        return Err(ApiError::unauthorized("missing X-Internal-Secret header"));
    }
    let matched = valid_secrets
        .iter()
        .any(|s| secret.as_bytes().ct_eq(s.as_bytes()).unwrap_u8() == 1);
    if !matched {
        return Err(ApiError::forbidden("invalid internal secret"));
    }
    Ok(())
}

/// Acquire a DB connection with a timeout. Returns 503 Service Unavailable
/// if the pool is exhausted, giving the caller a clear signal to retry.
pub(crate) async fn db_conn(pool: &deadpool_postgres::Pool) -> Result<deadpool_postgres::Object, ApiError> {
    match tokio::time::timeout(DB_POOL_TIMEOUT, pool.get()).await {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "db pool error");
            Err(ApiError::internal("database connection failed"))
        }
        Err(_) => {
            tracing::warn!("db pool timeout — all connections busy");
            Err(ApiError::unavailable("database connection pool exhausted — try again"))
        }
    }
}

#[derive(Serialize)]
pub struct HealthResponse {
    status:   &'static str,
    db:       &'static str,
    nats:     &'static str,
}

/// GET /healthz — probes DB and NATS, returns 200 only when both are reachable.
pub async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<HealthResponse>) {
    // DB probe — simple query
    let db_ok = match state.db.get().await {
        Ok(conn) => conn.query_one("SELECT 1", &[]).await.is_ok(),
        Err(_)   => false,
    };

    // NATS probe — flush pending writes (PING/PONG round-trip) with 2s timeout
    let nats_ok = tokio::time::timeout(
        Duration::from_secs(2),
        state.nats_client.flush(),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);

    let status = if db_ok && nats_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };

    (status, Json(HealthResponse {
        status: if db_ok && nats_ok { "ok" } else { "degraded" },
        db:     if db_ok   { "ok" } else { "error" },
        nats:   if nats_ok { "ok" } else { "error" },
    }))
}

// ── Auth middleware ───────────────────────────────────────────────────────────

/// Per-user auth middleware: validates the caller's UUID from either
/// `X-Api-Key: {uuid}` (CLI) or `Authorization: Bearer {uuid}` (web/ConnectRPC)
/// against the `users` table.
///
/// Set `DISABLE_AUTH=1` to bypass in local dev. Never set this in production.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if state.features.disable_auth {
        return Ok(next.run(req).await);
    }

    // Accept X-Api-Key (CLI) or Authorization: Bearer (web/ConnectRPC).
    let raw = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            req.headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        });

    let raw = raw.ok_or_else(|| ApiError::unauthorized("missing X-Api-Key or Authorization header"))?;
    let user_id: Uuid = raw.parse().map_err(|_| ApiError::unauthorized("malformed API key"))?;

    let db = db_conn(&state.db).await?;

    let row = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at IS NULL)",
            &[&user_id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "user lookup in auth middleware failed");
            ApiError::internal("authentication check failed")
        })?;

    if !row.get::<_, bool>(0) {
        tracing::warn!(target: "audit", action = "auth", user_id = %user_id, result = "unauthorized", "unknown or deleted user");
        return Err(ApiError::unauthorized("unknown or deleted user"));
    }

    Ok(next.run(req).await)
}

// ── Auth: public CLI config ───────────────────────────────────────────────────

/// GET /auth/cli/config — returns the OIDC endpoints and client_id for device flow.
///
/// None of these values are secrets (standard public OAuth identifiers).
/// The CLI fetches them at login time so no credentials are baked into the binary,
/// and swapping auth providers requires only env var changes — no binary rebuild.
pub async fn cli_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut cfg = serde_json::json!({
        "client_id":       state.oidc_client_id,
        "device_auth_url": state.oidc_device_auth_url,
        "token_url":       state.oidc_token_url,
        "userinfo_url":    state.oidc_userinfo_url,
    });
    if let Some(url) = &state.oidc_revoke_url {
        cfg["revoke_url"] = serde_json::Value::String(url.clone());
    }
    Json(cfg)
}

// ── Auth: shared types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ProvisionRequest {
    pub email:       String,
    pub first_name:  String,
    pub last_name:   String,
    pub oidc_sub: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProvisionResponse {
    pub id:           String,
    pub name:         String,
    pub slug:         String,
    pub tier:         String,
    pub workspace_id: String,
    pub oidc_sub:  Option<String>,
}

// ── Auth: internal provision (called by Go web BFF) ──────────────────────────

/// POST /auth/provision — upsert user + workspace on first browser login.
/// Protected by X-Internal-Secret header (Go web BFF → Rust API only).
pub async fn provision_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ProvisionRequest>,
) -> Result<Json<ProvisionResponse>, ApiError> {
    if let Err(e) = verify_internal_secret(&headers, &state.internal_secrets) {
        tracing::warn!(target: "audit", action = "provision_user", email = req.email, result = "forbidden", "invalid internal secret");
        return Err(e);
    }

    if req.email.is_empty() {
        return Err(ApiError::bad_request("missing_email", "email is required"));
    }

    let mut db = db_conn(&state.db).await?;

    let resp = do_provision(
        &mut db,
        &state.features,
        &req.email,
        &req.first_name,
        &req.last_name,
        req.oidc_sub.as_deref(),
        None, // web BFF path — no invite required (internal-secret gated)
    ).await;

    if let Ok(ref r) = resp {
        tracing::info!(target: "audit", action = "provision_user", user_id = r.id, email = req.email, result = "ok");
    }
    resp
}

// ── Auth: CLI provision (PKCE callback) ──────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CliProvisionRequest {
    pub email:       String,
    pub first_name:  String,
    pub last_name:   String,
    pub oidc_sub: Option<String>,
    pub invite_code: Option<String>,
}

/// POST /auth/cli/provision — provision a user after device flow login.
/// New users must supply a valid invite_code. Returning users skip the check.
pub async fn cli_provision(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CliProvisionRequest>,
) -> Result<Json<ProvisionResponse>, ApiError> {
    if req.email.is_empty() {
        return Err(ApiError::bad_request("missing_email", "email is required"));
    }
    let mut db = db_conn(&state.db).await?;
    let resp = do_provision(
        &mut db,
        &state.features,
        &req.email,
        &req.first_name,
        &req.last_name,
        req.oidc_sub.as_deref(),
        req.invite_code.as_deref(),
    ).await;

    match &resp {
        Ok(r) => tracing::info!(target: "audit", action = "cli_provision", user_id = r.id, email = req.email, result = "ok"),
        Err(e) => tracing::warn!(target: "audit", action = "cli_provision", email = req.email, status = e.status.as_u16(), result = "denied"),
    }
    resp
}

// ── Auth: shared provisioning logic ──────────────────────────────────────────

/// Upsert user + workspace atomically. Returns the user's UUID, display name,
/// workspace slug, and tier. Idempotent — safe to call on every login.
async fn do_provision(
    db:          &mut deadpool_postgres::Object,
    features:    &common::Features,
    email:       &str,
    first_name:  &str,
    last_name:   &str,
    oidc_sub: Option<&str>,
    invite_code: Option<&str>,
) -> Result<Json<ProvisionResponse>, ApiError> {
    // Upsert oidc_sub on the existing user if provided.
    if let Some(sub) = oidc_sub {
        db.execute(
            "UPDATE users SET oidc_sub = $1 WHERE email = $2 AND deleted_at IS NULL",
            &[&sub, &email],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "update oidc_sub");
            ApiError::internal("failed to update OIDC subject")
        })?;
    }

    // Fast path: return existing user (no invite check — they already provisioned).
    let existing = db
        .query_opt(
            "SELECT u.id, u.name, u.tier, u.oidc_sub, w.id AS workspace_id, w.slug \
             FROM users u \
             LEFT JOIN workspace_members wm ON wm.user_id = u.id AND wm.role = 'owner' \
             LEFT JOIN workspaces w ON w.id = wm.workspace_id \
             WHERE u.email = $1 AND u.deleted_at IS NULL",
            &[&email],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "provision lookup");
            ApiError::internal("user lookup failed")
        })?;

    if let Some(row) = existing {
        return Ok(Json(ProvisionResponse {
            id:           row.get::<_, Uuid>("id").to_string(),
            name:         row.get("name"),
            slug:         row.get::<_, Option<String>>("slug").unwrap_or_default(),
            tier:         row.get::<_, Option<String>>("tier")
                              .unwrap_or_else(|| "hobby".to_string()),
            workspace_id: row.get::<_, Option<Uuid>>("workspace_id")
                              .map(|u| u.to_string())
                              .unwrap_or_default(),
            oidc_sub:  row.get("oidc_sub"),
        }));
    }

    // New user — validate invite code before creating anything.
    if features.require_invite {
        match invite_code {
            Some(code) => {
                let valid = db
                    .query_opt(
                        "SELECT code FROM invite_codes WHERE code = $1 AND used_by IS NULL",
                        &[&code],
                    )
                    .await
                    .map_err(|e| {
                        tracing::error!(error = %e, "invite code lookup");
                        ApiError::internal("invite code lookup failed")
                    })?;
                if valid.is_none() {
                    tracing::warn!(code, "invalid or already-used invite code");
                    return Err(ApiError::forbidden("invalid or already-used invite code"));
                }
            }
            None => {
                tracing::warn!(email, "new user attempted signup without invite code");
                return Err(ApiError::forbidden("invite code required for new accounts"));
            }
        }
    }

    // Create user + workspace + mark invite used — all in one transaction.
    let user_id      = Uuid::now_v7();
    let workspace_id = Uuid::now_v7();
    let full_name    = format!("{} {}", first_name, last_name).trim().to_string();
    let full_name    = if full_name.is_empty() { email.to_string() } else { full_name };
    let ws_slug      = workspace_slug(&full_name);

    let txn = db.transaction().await.map_err(|e| {
        tracing::error!(error = %e, "begin txn");
        ApiError::internal("failed to start transaction")
    })?;

    txn.execute(
        "INSERT INTO users (id, email, name, oidc_sub) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (email) DO UPDATE SET oidc_sub = EXCLUDED.oidc_sub \
         WHERE EXCLUDED.oidc_sub IS NOT NULL",
        &[&user_id, &email, &full_name, &oidc_sub],
    ).await.map_err(|e| {
        tracing::error!(error = ?e, "insert user");
        ApiError::internal("failed to create user")
    })?;

    txn.execute(
        "INSERT INTO workspaces (id, name, slug) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        &[&workspace_id, &"My Workspace".to_string(), &ws_slug],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "insert workspace");
        ApiError::internal("failed to create workspace")
    })?;

    txn.execute(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
         VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING",
        &[&workspace_id, &user_id],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "insert workspace member");
        ApiError::internal("failed to create workspace membership")
    })?;

    // Consume the invite code atomically with user creation.
    if let Some(code) = invite_code {
        txn.execute(
            "UPDATE invite_codes SET used_by = $1, used_at = now() WHERE code = $2",
            &[&user_id, &code],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "consume invite code");
            ApiError::internal("failed to consume invite code")
        })?;
    }

    txn.commit().await.map_err(|e| {
        tracing::error!(error = %e, "commit txn");
        ApiError::internal("failed to commit user creation")
    })?;

    tracing::info!(email, user_id = %user_id, "user provisioned");

    Ok(Json(ProvisionResponse {
        id:           user_id.to_string(),
        name:         full_name,
        slug:         ws_slug,
        tier:         "hobby".to_string(),
        workspace_id: workspace_id.to_string(),
        oidc_sub:  oidc_sub.map(|s| s.to_string()),
    }))
}

// ── Admin: invite code generation ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateInvitesRequest {
    pub count: Option<i64>, // default 1, max 50
}

#[derive(Serialize)]
pub struct CreateInvitesResponse {
    pub codes: Vec<String>,
}

/// POST /admin/invites — generate single-use invite codes.
/// Protected by X-Internal-Secret. Run `flux invite generate` to call this.
pub async fn create_invites(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateInvitesRequest>,
) -> Result<Json<CreateInvitesResponse>, ApiError> {
    if let Err(e) = verify_internal_secret(&headers, &state.internal_secrets) {
        tracing::warn!(target: "audit", action = "create_invites", result = "forbidden", "invalid internal secret");
        return Err(e);
    }

    let count = req.count.unwrap_or(1).clamp(1, 50) as usize;
    let db = db_conn(&state.db).await?;

    let mut codes = Vec::with_capacity(count);
    for _ in 0..count {
        let code = generate_invite_code();
        db.execute(
            "INSERT INTO invite_codes (code) VALUES ($1)",
            &[&code],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "insert invite code");
            ApiError::internal("failed to create invite code")
        })?;
        codes.push(code);
    }

    tracing::info!(target: "audit", action = "create_invites", count, result = "ok");
    Ok(Json(CreateInvitesResponse { codes }))
}

/// Generate a human-readable invite code: XXXX-XXXX (hex, uppercase).
fn generate_invite_code() -> String {
    let id = Uuid::new_v4().to_string().replace('-', "");
    let upper = id[..8].to_uppercase();
    format!("{}-{}", &upper[..4], &upper[4..8])
}


/// Append "-workspace" suffix to a slugified display name.
fn workspace_slug(name: &str) -> String {
    format!("{}-workspace", common::slugify(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::slugify;

    // ── slugify (via common) ──────────────────────────────────────────────────

    #[test]
    fn slugify_lowercase() {
        assert_eq!(slugify("MyApp"), "myapp");
    }

    #[test]
    fn slugify_spaces_become_dashes() {
        assert_eq!(slugify("my app"), "my-app");
    }

    #[test]
    fn slugify_collapses_consecutive_dashes() {
        assert_eq!(slugify("my--app"), "my-app");
    }

    #[test]
    fn slugify_trims_leading_trailing_dashes() {
        assert_eq!(slugify("  my app  "), "my-app");
    }

    #[test]
    fn slugify_special_chars() {
        assert_eq!(slugify("hello/world!"), "hello-world");
    }

    #[test]
    fn slugify_empty() {
        assert_eq!(slugify(""), "");
    }

    // ── workspace_slug ────────────────────────────────────────────────────────

    #[test]
    fn workspace_slug_appends_suffix() {
        assert_eq!(workspace_slug("Alice Smith"), "alice-smith-workspace");
    }

    #[test]
    fn workspace_slug_collapses_dashes() {
        // Multiple spaces → multiple dashes → collapsed to one
        assert_eq!(workspace_slug("Alice  Smith"), "alice-smith-workspace");
        assert_eq!(workspace_slug("Alice   Smith"), "alice-smith-workspace");
    }

    #[test]
    fn workspace_slug_email_fallback() {
        // Single-word input (e.g. email used as name) still gets -workspace suffix
        let s = workspace_slug("alice@example.com");
        assert!(s.ends_with("-workspace"));
    }
}

// ── REST: shared auth helper ───────────────────────────────────────────────────

/// Extract the caller's UUID from X-Api-Key or Authorization: Bearer.
/// The auth_middleware has already validated the UUID exists in the DB,
/// so here we only need to parse it.
fn extract_caller(headers: &HeaderMap) -> Result<Uuid, ApiError> {
    let raw = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
        .ok_or_else(|| ApiError::unauthorized("missing X-Api-Key or Authorization header"))?;
    raw.parse().map_err(|_| ApiError::unauthorized("malformed API key"))
}

// ── GET /users/me ─────────────────────────────────────────────────────────────

pub async fn get_me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = extract_caller(&headers)?;
    let db = db_conn(&state.db).await?;

    let row = db
        .query_opt(
            "SELECT id, email, name FROM users WHERE id = $1 AND deleted_at IS NULL",
            &[&caller],
        )
        .await
        .map_err(|_| ApiError::internal("user lookup failed"))?
        .ok_or_else(|| ApiError::not_found("user not found"))?;

    Ok(Json(serde_json::json!({
        "id":    row.get::<_, Uuid>("id").to_string(),
        "email": row.get::<_, String>("email"),
        "name":  row.get::<_, String>("name"),
    })))
}

// ── GET /workspaces ───────────────────────────────────────────────────────────

pub async fn list_workspaces(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = extract_caller(&headers)?;
    let db = db_conn(&state.db).await?;

    let rows = db
        .query(
            "SELECT w.id, w.name, w.slug, w.tier \
             FROM workspaces w \
             JOIN workspace_members wm ON wm.workspace_id = w.id AND wm.user_id = $1 \
             WHERE w.deleted_at IS NULL \
             ORDER BY w.created_at ASC \
             LIMIT 50",
            &[&caller],
        )
        .await
        .map_err(|_| ApiError::internal("failed to list workspaces"))?;

    let workspaces: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| serde_json::json!({
            "id":   row.get::<_, Uuid>("id").to_string(),
            "name": row.get::<_, String>("name"),
            "slug": row.get::<_, String>("slug"),
            "tier": row.get::<_, String>("tier"),
        }))
        .collect();

    Ok(Json(serde_json::Value::Array(workspaces)))
}

// ── GET /projects?workspace_id=<uuid> ────────────────────────────────────────

#[derive(Deserialize)]
pub struct ListProjectsParams {
    workspace_id: String,
}

pub async fn list_projects(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<ListProjectsParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = extract_caller(&headers)?;
    let wid: Uuid = params.workspace_id.parse().map_err(|_| ApiError::bad_request("invalid_workspace_id", "workspace_id must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;

    let rows = db
        .query(
            "SELECT p.id, p.workspace_id, p.name, p.slug \
             FROM projects p \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE p.workspace_id = $1 AND p.deleted_at IS NULL \
             ORDER BY p.created_at DESC \
             LIMIT 200",
            &[&wid, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("failed to list projects"))?;

    let projects: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| serde_json::json!({
            "id":           row.get::<_, Uuid>("id").to_string(),
            "workspace_id": row.get::<_, Uuid>("workspace_id").to_string(),
            "name":         row.get::<_, String>("name"),
            "slug":         row.get::<_, String>("slug"),
        }))
        .collect();

    Ok(Json(serde_json::Value::Array(projects)))
}

// ── POST /projects ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateProjectBody {
    workspace_id: String,
    name: String,
    slug: String,
}

pub async fn create_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateProjectBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = extract_caller(&headers)?;

    if body.name.is_empty() || body.slug.is_empty() || body.workspace_id.is_empty() {
        return Err(ApiError::bad_request("missing_fields", "name, slug, and workspace_id are required"));
    }

    let wid: Uuid = body.workspace_id.parse().map_err(|_| ApiError::bad_request("invalid_workspace_id", "workspace_id must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;

    let member = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
            &[&wid, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("workspace membership check failed"))?;

    if !member.get::<_, bool>(0) {
        return Err(ApiError::forbidden("not a member of this workspace"));
    }

    let project_id = Uuid::now_v7();

    db.execute(
        "INSERT INTO projects (id, workspace_id, name, slug) VALUES ($1, $2, $3, $4)",
        &[&project_id, &wid, &body.name, &body.slug],
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert project failed");
        if e.as_db_error()
            .map(|d| d.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
            .unwrap_or(false)
        {
            ApiError::conflict("a project with this slug already exists in this workspace")
        } else {
            ApiError::internal("failed to create project")
        }
    })?;

    Ok(Json(serde_json::json!({
        "project": {
            "id":           project_id.to_string(),
            "workspace_id": wid.to_string(),
            "name":         body.name,
            "slug":         body.slug,
        }
    })))
}

// ── GET /services ─────────────────────────────────────────────────────────────

pub async fn list_services(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = extract_caller(&headers)?;
    let db = db_conn(&state.db).await?;

    let rows = db
        .query(
            "SELECT s.id, s.name, s.slug, s.engine, s.status, s.upstream_addr \
             FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $1 \
             WHERE s.deleted_at IS NULL \
             ORDER BY s.created_at DESC \
             LIMIT 500",
            &[&caller],
        )
        .await
        .map_err(|_| ApiError::internal("failed to list services"))?;

    let services: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| serde_json::json!({
            "id":            row.get::<_, Uuid>("id").to_string(),
            "name":          row.get::<_, String>("name"),
            "slug":          row.get::<_, String>("slug"),
            "engine":        row.get::<_, String>("engine"),
            "status":        row.get::<_, String>("status"),
            "upstream_addr": row.get::<_, Option<String>>("upstream_addr").unwrap_or_default(),
        }))
        .collect();

    Ok(Json(serde_json::Value::Array(services)))
}

// ── POST /deployments/upload-url ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct UploadUrlBody {
    engine: String,
    deploy_id: String,
    project_id: String,
}

pub async fn get_upload_url(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<UploadUrlBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = extract_caller(&headers)?;

    if body.deploy_id.is_empty() || body.project_id.is_empty() {
        return Err(ApiError::bad_request("missing_fields", "deploy_id and project_id are required"));
    }

    let pid: Uuid = body.project_id.parse().map_err(|_| ApiError::bad_request("invalid_project_id", "project_id must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;

    let row = db
        .query_opt(
            "SELECT workspace_id FROM projects WHERE id = $1 AND deleted_at IS NULL",
            &[&pid],
        )
        .await
        .map_err(|_| ApiError::internal("project lookup failed"))?
        .ok_or_else(|| ApiError::not_found("project not found"))?;

    let wid: Uuid = row.get("workspace_id");

    let member = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
            &[&wid, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("workspace membership check failed"))?;

    if !member.get::<_, bool>(0) {
        return Err(ApiError::forbidden("not a member of this workspace"));
    }

    let (engine_prefix, artifact_name) = match body.engine.as_str() {
        "liquid" => ("wasm", "main.wasm"),
        "metal"  => ("metal", "app"),
        _        => return Err(ApiError::bad_request("invalid_engine", "engine must be 'metal' or 'liquid'")),
    };

    let artifact_key = format!(
        "{}/{}/{}/{}",
        engine_prefix, body.project_id, body.deploy_id, artifact_name
    );

    // Metal rootfs images can be 500MB+ — give slow connections 30 minutes.
    // Wasm modules are small; 5 minutes is plenty.
    let expiry_secs = if body.engine == "metal" { 1800 } else { 300 };
    let expires = aws_sdk_s3::presigning::PresigningConfig::expires_in(
        std::time::Duration::from_secs(expiry_secs),
    )
    .map_err(|_| ApiError::internal("presign config error"))?;

    let presigned = state
        .s3
        .put_object()
        .bucket(&state.bucket)
        .key(&artifact_key)
        .presigned(expires)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "S3 presign failed");
            ApiError::internal("failed to generate upload URL")
        })?;

    Ok(Json(serde_json::json!({
        "upload_url":   presigned.uri().to_string(),
        "artifact_key": artifact_key,
    })))
}

// ── POST /deployments ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeployBody {
    name: String,
    slug: String,
    engine: String,
    project_id: String,
    artifact_key: String,
    sha256: String,
    // Metal-only (optional for liquid deploys)
    vcpu: Option<u32>,
    memory_mb: Option<u32>,
    port: Option<u32>,
}

pub async fn deploy_service(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<DeployBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = extract_caller(&headers)?;

    if body.name.is_empty() {
        return Err(ApiError::bad_request("missing_name", "service name is required"));
    }
    if body.name.len() > 63 {
        return Err(ApiError::bad_request("name_too_long", "service name must be 63 characters or fewer"));
    }
    if body.artifact_key.is_empty() {
        return Err(ApiError::bad_request("missing_artifact_key", "artifact_key is required"));
    }

    let pid: Uuid = body.project_id.parse().map_err(|_| ApiError::bad_request("invalid_project_id", "project_id must be a valid UUID"))?;
    let mut db = db_conn(&state.db).await?;

    let row = db
        .query_opt(
            "SELECT workspace_id FROM projects WHERE id = $1 AND deleted_at IS NULL",
            &[&pid],
        )
        .await
        .map_err(|_| ApiError::internal("project lookup failed"))?
        .ok_or_else(|| ApiError::not_found("project not found"))?;

    let wid: Uuid = row.get("workspace_id");

    let member = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
            &[&wid, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("workspace membership check failed"))?;

    if !member.get::<_, bool>(0) {
        return Err(ApiError::forbidden("not a member of this workspace"));
    }

    let tier_row = db
        .query_one("SELECT tier FROM workspaces WHERE id = $1", &[&wid])
        .await
        .map_err(|_| ApiError::internal("workspace tier lookup failed"))?;
    let tier: String = tier_row.get("tier");
    let limits = crate::quota::limits_for(&tier);

    let engine: common::Engine = body.engine.parse()
        .map_err(|_| ApiError::bad_request("invalid_engine", "engine must be 'metal' or 'liquid'"))?;

    let engine_spec = match engine {
        common::Engine::Metal => {
            let vcpu      = body.vcpu.unwrap_or(0);
            let memory_mb = body.memory_mb.unwrap_or(0);
            let port      = body.port.unwrap_or(0) as u16;
            if vcpu == 0 || memory_mb < 64 || port == 0 {
                return Err(ApiError::bad_request("invalid_metal_spec", "metal deploys require vcpu >= 1, memory_mb >= 64, and port >= 1"));
            }
            if vcpu > limits.max_vcpu {
                return Err(ApiError::unprocessable("vcpu_limit_exceeded", format!("vcpu {} exceeds {} tier limit of {}", vcpu, tier, limits.max_vcpu)));
            }
            if memory_mb > limits.max_memory_mb {
                return Err(ApiError::unprocessable("memory_limit_exceeded", format!("memory_mb {} exceeds {} tier limit of {}", memory_mb, tier, limits.max_memory_mb)));
            }
            common::EngineSpec::Metal(common::MetalSpec {
                vcpu,
                memory_mb,
                port,
                artifact_key:    body.artifact_key.clone(),
                artifact_sha256: if body.sha256.is_empty() { None } else { Some(body.sha256.clone()) },
                quota:           Default::default(),
            })
        }
        common::Engine::Liquid => common::EngineSpec::Liquid(common::LiquidSpec {
            artifact_key:    body.artifact_key.clone(),
            artifact_sha256: if body.sha256.is_empty() { None } else { Some(body.sha256.clone()) },
        }),
    };

    let (vcpu_i, memory_mb_i, port_i) = match &engine_spec {
        common::EngineSpec::Metal(m) => (m.vcpu as i32, m.memory_mb as i32, m.port as i32),
        common::EngineSpec::Liquid(_) => (0i32, 0i32, 0i32),
    };

    let engine_str = engine.as_str();

    let service_id = Uuid::now_v7();
    let slug = if body.slug.is_empty() { common::slugify(&body.name) } else { body.slug.clone() };

    // ── Capacity & limit checks (short-lived advisory lock) ────────────────
    // Acquire a workspace-scoped advisory lock for just the capacity and service
    // count checks, then release it immediately by committing. This prevents
    // TOCTOU races while avoiding serialization of the heavier INSERT + outbox
    // write below. A CI pipeline deploying 20 services in parallel will only
    // serialize on the checks (~1ms each), not the full deploy transaction.
    //
    // Trade-off: between lock release and INSERT, another deploy could also pass
    // the checks, potentially exceeding the limit by 1. This is acceptable —
    // the limit is a soft gate, not a billing boundary.
    let lock_key = {
        let b = wid.as_bytes();
        i64::from_be_bytes(b[0..8].try_into().unwrap())
            ^ i64::from_be_bytes(b[8..16].try_into().unwrap())
    };

    {
        let check_txn = db
            .build_transaction()
            .start()
            .await
            .map_err(|_| ApiError::internal("failed to start check transaction"))?;

        check_txn.execute("SELECT pg_advisory_xact_lock($1)", &[&lock_key])
            .await
            .map_err(|_| ApiError::internal("failed to acquire workspace lock"))?;

        // Hobby tier capacity gate: reject Metal deploys when >80% of cluster RAM is allocated.
        if engine == common::Engine::Metal && tier == "hobby" && state.metal_capacity_mb > 0 {
            let row = check_txn
                .query_one(
                    "SELECT COALESCE(SUM(memory_mb), 0)::bigint AS allocated_mb \
                     FROM services \
                     WHERE engine = 'metal' \
                       AND status IN ('running', 'provisioning') \
                       AND deleted_at IS NULL",
                    &[],
                )
                .await
                .map_err(|_| ApiError::internal("capacity check failed"))?;
            let allocated_mb: i64 = row.get("allocated_mb");
            if allocated_mb * 10 > state.metal_capacity_mb * HOBBY_CAPACITY_THRESHOLD_NUMERATOR {
                tracing::warn!(
                    allocated_mb,
                    capacity_mb = state.metal_capacity_mb,
                    "hobby metal deploy rejected — nodes >80% allocated"
                );
                return Err(ApiError::unavailable("cluster capacity exceeded — try again later or upgrade your plan"));
            }
        }

        let svc_count: i64 = check_txn
            .query_one(
                "SELECT COUNT(*) FROM services WHERE workspace_id = $1 AND deleted_at IS NULL",
                &[&wid],
            )
            .await
            .map_err(|_| ApiError::internal("service count query failed"))?
            .get(0);

        if svc_count >= limits.max_services {
            return Err(ApiError::unprocessable("service_limit_exceeded", format!("workspace has {} services, {} tier limit is {}", svc_count, tier, limits.max_services)));
        }

        // Reject deploy if there's already an active service with this slug in a
        // non-terminal state. Prevents the stop-then-deploy race where a
        // DeprovisionEvent is in-flight while the new ProvisionEvent is queued —
        // the deprovision's RouteRemovedEvent would briefly evict the new route.
        let active_slug: bool = check_txn
            .query_one(
                "SELECT EXISTS(\
                   SELECT 1 FROM services \
                   WHERE workspace_id = $1 AND slug = $2 \
                     AND status IN ('running', 'provisioning') \
                     AND deleted_at IS NULL\
                 )",
                &[&wid, &slug],
            )
            .await
            .map_err(|_| ApiError::internal("active slug check failed"))?
            .get(0);

        if active_slug {
            return Err(ApiError::conflict(
                "a service with this slug is currently running or provisioning — stop it first",
            ));
        }

        // Commit releases the advisory lock — other deploys for this workspace
        // can now proceed with their own checks.
        check_txn.commit().await.map_err(|_| ApiError::internal("check transaction commit failed"))?;
    }

    // ── Deploy transaction (no advisory lock) ────────────────────────────────
    let txn = db
        .build_transaction()
        .start()
        .await
        .map_err(|_| ApiError::internal("failed to start transaction"))?;

    // Clean up previous service with the same slug (redeploy). Soft-delete the
    // old row and queue its S3 artifact for deletion so storage doesn't leak.
    let old_artifact = txn
        .query_opt(
            "UPDATE services \
             SET deleted_at = NOW() \
             WHERE workspace_id = $1 AND slug = $2 \
               AND deleted_at IS NULL AND status IN ('stopped', 'failed') \
             RETURNING artifact_key",
            &[&wid, &slug],
        )
        .await
        .map_err(|_| ApiError::internal("old service cleanup failed"))?
        .and_then(|r| r.get::<_, Option<String>>("artifact_key"));

    txn.execute(
        "INSERT INTO services \
         (id, project_id, workspace_id, name, slug, engine, status, \
          artifact_key, commit_sha, vcpu, memory_mb, port) \
         VALUES ($1, $2, $3, $4, $5, $6, 'provisioning', $7, $8, $9, $10, $11)",
        &[
            &service_id, &pid, &wid,
            &body.name, &slug, &engine_str,
            &body.artifact_key, &body.sha256,
            &vcpu_i, &memory_mb_i, &port_i,
        ],
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "service insert failed");
        ApiError::internal("failed to create service")
    })?;

    let event = common::ProvisionEvent {
        tenant_id:  wid.to_string(),
        service_id: service_id.to_string(),
        app_name:   body.name.clone(),
        slug:       slug.clone(),
        engine:     engine.clone(),
        spec:       engine_spec,
    };

    // Write the NATS event into the outbox within the same transaction as the
    // services INSERT. Commit atomically — either both land or neither does.
    // The outbox poller (running in the background) will publish to NATS and
    // delete the row once it receives a JetStream ack. This eliminates the
    // race where a DB commit succeeds but the NATS publish fails.
    let payload = serde_json::to_value(&event).map_err(|e| {
        tracing::error!(error = %e, "serializing provision event");
        ApiError::internal("failed to serialize provision event")
    })?;
    txn.execute(
        "INSERT INTO outbox (subject, payload) VALUES ($1, $2)",
        &[&common::events::SUBJECT_PROVISION, &payload],
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "outbox insert failed");
        ApiError::internal("failed to queue provision event")
    })?;

    txn.commit().await.map_err(|_| ApiError::internal("failed to commit deploy transaction"))?;

    // Fire-and-forget: delete the old artifact from S3 after commit.
    // Non-blocking — a failure here only wastes storage, not correctness.
    if let Some(old_key) = old_artifact {
        let s3 = state.s3.clone();
        let bucket = state.bucket.clone();
        tokio::spawn(async move {
            if let Err(e) = s3.delete_object().bucket(&bucket).key(&old_key).send().await {
                tracing::warn!(key = old_key, error = %e, "failed to delete old artifact from S3");
            } else {
                tracing::debug!(key = old_key, "deleted old artifact from S3");
            }
        });
    }

    tracing::info!(
        target: "audit",
        action = "deploy_service",
        user_id = %caller,
        service_id = %service_id,
        slug,
        engine = engine_str,
        workspace_id = %wid,
        result = "ok",
    );

    Ok(Json(serde_json::json!({
        "service": {
            "id":     service_id.to_string(),
            "name":   body.name,
            "slug":   slug,
            "status": "provisioning",
        }
    })))
}

// ── GET /services/:id/logs ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LogsParams {
    limit: Option<i64>,
}

pub async fn get_service_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<LogsParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = extract_caller(&headers)?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;
    let limit = params.limit.unwrap_or(100).clamp(1, 1000);

    let db = db_conn(&state.db).await?;

    // Verify caller has access and fetch the service slug for VictoriaLogs query.
    let row = db
        .query_opt(
            "SELECT s.slug FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?;

    let slug: String = match row {
        Some(r) => r.get("slug"),
        None => return Err(ApiError::not_found("service not found")),
    };

    // Query VictoriaLogs for this service's logs (collected by Promtail).
    let vlogs_url = &state.victorialogs_url;
    if vlogs_url.is_empty() {
        return Ok(Json(serde_json::Value::Array(vec![])));
    }

    let query = format!("task:\"{}\"", slug.replace('"', ""));
    let resp = state.http_client
        .get(format!("{vlogs_url}/select/logsql/query"))
        .query(&[("query", &query), ("limit", &limit.to_string())])
        .send()
        .await
        .map_err(|_| ApiError::bad_gateway("log backend unreachable"))?;

    if !resp.status().is_success() {
        tracing::warn!(status = %resp.status(), slug, "VictoriaLogs query failed");
        return Err(ApiError::bad_gateway("log query failed"));
    }

    // VictoriaLogs returns newline-delimited JSON. Each line is a log entry.
    let body = resp.text().await.map_err(|_| ApiError::bad_gateway("failed to read log response"))?;
    let lines: Vec<serde_json::Value> = body
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let obj: serde_json::Value = serde_json::from_str(l).ok()?;
            Some(serde_json::json!({
                "ts":      obj.get("_time"),
                "message": obj.get("_msg").or_else(|| obj.get("message")).unwrap_or(&serde_json::Value::Null),
            }))
        })
        .collect();

    Ok(Json(serde_json::Value::Array(lines)))
}

// ── POST /services/:id/stop ───────────────────────────────────────────────────

pub async fn stop_service(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let caller = extract_caller(&headers)?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;

    let db = db_conn(&state.db).await?;

    // First check the service exists and caller has access.
    let existing = db
        .query_opt(
            "SELECT s.status FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let current_status: String = existing.get("status");
    if current_status == "stopped" {
        return Err(ApiError::conflict("service is already stopped"));
    }

    let row = db
        .query_opt(
            "UPDATE services s \
             SET status = 'stopped', upstream_addr = NULL \
             FROM projects p, workspace_members wm \
             WHERE s.id = $1 \
               AND s.deleted_at IS NULL \
               AND s.status != 'stopped' \
               AND s.project_id = p.id \
               AND wm.workspace_id = p.workspace_id \
               AND wm.user_id = $2 \
             RETURNING s.engine, s.slug",
            &[&service_id, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("failed to stop service"))?
        .ok_or_else(|| ApiError::conflict("service state changed concurrently"))?;

    let engine_str: String = row.get("engine");
    let slug: String       = row.get("slug");
    let engine: common::Engine = engine_str.parse().map_err(|_| {
        tracing::error!(engine = engine_str, service_id = %service_id, "unknown engine in DB");
        ApiError::internal("corrupt engine value in database")
    })?;

    // Remove any pending outbox rows for this service — if a provision event
    // hasn't been published yet, we don't want it replayed after the user stops.
    db.execute(
        "DELETE FROM outbox WHERE payload->>'service_id' = $1",
        &[&service_id.to_string()],
    )
    .await
    .ok();

    let event = common::events::DeprovisionEvent {
        service_id: service_id.to_string(),
        slug,
        engine,
    };
    crate::nats::publish_deprovision(&state.nats, &event)
        .await
        .map_err(|e| tracing::error!(error = %e, "nats deprovision publish failed"))
        .ok();

    tracing::info!(
        target: "audit",
        action = "stop_service",
        user_id = %caller,
        service_id = %service_id,
        slug = event.slug,
        result = "ok",
    );

    Ok(StatusCode::NO_CONTENT)
}

// ── POST /services/:id/restart ────────────────────────────────────────────────

pub async fn restart_service(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = extract_caller(&headers)?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;

    let db = db_conn(&state.db).await?;

    // First check the service exists and caller has access.
    let existing = db
        .query_opt(
            "SELECT s.status FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let current_status: String = existing.get("status");
    if current_status != "stopped" && current_status != "failed" {
        return Err(ApiError::conflict(format!("service is currently '{}' — only stopped or failed services can be restarted", current_status)));
    }

    let row = db
        .query_opt(
            "SELECT s.id, s.name, s.slug, s.engine, s.workspace_id, \
                    s.vcpu, s.memory_mb, s.port, s.artifact_key, s.commit_sha \
             FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 \
               AND s.deleted_at IS NULL \
               AND s.status IN ('stopped', 'failed')",
            &[&service_id, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::conflict("service state changed concurrently"))?;

    let engine_str: String = row.get("engine");
    let engine: common::Engine = engine_str.parse().map_err(|_| {
        tracing::error!(engine = engine_str, service_id = %service_id, "unknown engine in DB");
        ApiError::internal("corrupt engine value in database")
    })?;
    let wid: Uuid           = row.get("workspace_id");
    let name: String        = row.get("name");
    let slug: String        = row.get("slug");
    let artifact_key: String = row.get::<_, Option<String>>("artifact_key").unwrap_or_default();

    let spec = match engine {
        common::Engine::Metal => common::EngineSpec::Metal(common::MetalSpec {
            vcpu:            row.get::<_, i32>("vcpu") as u32,
            memory_mb:       row.get::<_, i32>("memory_mb") as u32,
            port:            row.get::<_, i32>("port") as u16,
            artifact_key:    artifact_key,
            artifact_sha256: None,
            quota:           Default::default(),
        }),
        common::Engine::Liquid => common::EngineSpec::Liquid(common::LiquidSpec {
            artifact_key:    artifact_key,
            artifact_sha256: None,
        }),
    };

    db.execute(
        "UPDATE services SET status = 'provisioning', upstream_addr = NULL WHERE id = $1",
        &[&service_id],
    )
    .await
    .map_err(|_| ApiError::internal("failed to update service status"))?;

    let event = common::ProvisionEvent {
        tenant_id:  wid.to_string(),
        service_id: service_id.to_string(),
        app_name:   name.clone(),
        slug:       slug.clone(),
        engine:     engine,
        spec,
    };

    crate::nats::publish_provision(&state.nats, &event)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "nats provision publish failed");
            ApiError::internal("failed to publish restart event")
        })?;

    Ok(Json(serde_json::json!({
        "service": {
            "id":     service_id.to_string(),
            "name":   name,
            "slug":   slug,
            "status": "provisioning",
        }
    })))
}

// ── DELETE /workspaces/:id ────────────────────────────────────────────────────

/// Soft-delete a workspace and deprovision all its running services.
///
/// Steps:
/// 1. Verify caller is an owner of the workspace.
/// 2. Find all non-stopped, non-deleted services in the workspace.
/// 3. Mark them stopped and publish DeprovisionEvent for each.
/// 4. Soft-delete all projects and the workspace itself.
pub async fn delete_workspace(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let caller = extract_caller(&headers)?;
    let wid: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_workspace_id", "workspace ID must be a valid UUID"))?;

    let db = db_conn(&state.db).await?;

    // Verify caller owns this workspace.
    let owns = db
        .query_opt(
            "SELECT 1 FROM workspace_members \
             WHERE workspace_id = $1 AND user_id = $2 AND role = 'owner'",
            &[&wid, &caller],
        )
        .await
        .map_err(|_| ApiError::internal("ownership check failed"))?;

    if owns.is_none() {
        return Err(ApiError::not_found("workspace not found"));
    }

    // Find all running/provisioning services in this workspace.
    let running = db
        .query(
            "SELECT id::text, slug, engine FROM services \
             WHERE workspace_id = $1 AND deleted_at IS NULL \
               AND status IN ('running', 'provisioning')",
            &[&wid],
        )
        .await
        .map_err(|_| ApiError::internal("failed to list running services"))?;

    // Mark them stopped and publish deprovision events.
    for row in &running {
        let sid: String   = row.get("id");
        let slug: String  = row.get("slug");
        let eng: String   = row.get("engine");

        let engine: common::Engine = match eng.parse() {
            Ok(e) => e,
            Err(_) => {
                tracing::error!(engine = eng, service_id = sid, "unknown engine during workspace delete");
                continue;
            }
        };

        let event = common::events::DeprovisionEvent {
            service_id: sid.clone(),
            slug,
            engine,
        };

        crate::nats::publish_deprovision(&state.nats, &event)
            .await
            .map_err(|e| tracing::error!(error = %e, service_id = sid, "deprovision publish failed"))
            .ok();
    }

    // Soft-delete all services, projects, and the workspace in one shot.
    db.execute(
        "UPDATE services SET status = 'stopped', upstream_addr = NULL, deleted_at = NOW() \
         WHERE workspace_id = $1 AND deleted_at IS NULL",
        &[&wid],
    )
    .await
    .map_err(|_| ApiError::internal("failed to delete services"))?;

    db.execute(
        "UPDATE projects SET deleted_at = NOW() WHERE workspace_id = $1 AND deleted_at IS NULL",
        &[&wid],
    )
    .await
    .map_err(|_| ApiError::internal("failed to delete projects"))?;

    db.execute(
        "UPDATE workspaces SET deleted_at = NOW() WHERE id = $1 AND deleted_at IS NULL",
        &[&wid],
    )
    .await
    .map_err(|_| ApiError::internal("failed to delete workspace"))?;

    tracing::info!(
        target: "audit",
        action = "delete_workspace",
        user_id = %caller,
        workspace_id = %wid,
        services_deprovisioned = running.len(),
        result = "ok",
    );

    Ok(StatusCode::NO_CONTENT)
}
