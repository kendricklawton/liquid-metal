use crate::AppState;
use axum::{
    Json,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

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
) -> Result<Response, StatusCode> {
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

    let raw = raw.ok_or(StatusCode::UNAUTHORIZED)?;
    let user_id: Uuid = raw.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;

    let db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool error in auth middleware");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let row = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at IS NULL)",
            &[&user_id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "user lookup in auth middleware failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !row.get::<_, bool>(0) {
        return Err(StatusCode::UNAUTHORIZED);
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
) -> Result<Json<ProvisionResponse>, StatusCode> {
    let secret = headers
        .get("x-internal-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if secret != state.internal_secret {
        return Err(StatusCode::FORBIDDEN);
    }

    if req.email.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    do_provision(
        &mut db,
        &state.features,
        &req.email,
        &req.first_name,
        &req.last_name,
        req.oidc_sub.as_deref(),
        None, // web BFF path — no invite required (internal-secret gated)
    ).await
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
) -> Result<Json<ProvisionResponse>, StatusCode> {
    if req.email.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    do_provision(
        &mut db,
        &state.features,
        &req.email,
        &req.first_name,
        &req.last_name,
        req.oidc_sub.as_deref(),
        req.invite_code.as_deref(),
    ).await
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
) -> Result<Json<ProvisionResponse>, StatusCode> {
    // Upsert oidc_sub on the existing user if provided.
    if let Some(sub) = oidc_sub {
        db.execute(
            "UPDATE users SET oidc_sub = $1 WHERE email = $2 AND deleted_at IS NULL",
            &[&sub, &email],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "update oidc_sub");
            StatusCode::INTERNAL_SERVER_ERROR
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
            StatusCode::INTERNAL_SERVER_ERROR
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
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;
                if valid.is_none() {
                    tracing::warn!(code, "invalid or already-used invite code");
                    return Err(StatusCode::FORBIDDEN);
                }
            }
            None => {
                tracing::warn!(email, "new user attempted signup without invite code");
                return Err(StatusCode::FORBIDDEN);
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
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    txn.execute(
        "INSERT INTO users (id, email, name, oidc_sub) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (email) DO UPDATE SET oidc_sub = EXCLUDED.oidc_sub \
         WHERE EXCLUDED.oidc_sub IS NOT NULL",
        &[&user_id, &email, &full_name, &oidc_sub],
    ).await.map_err(|e| {
        tracing::error!(error = ?e, "insert user");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    txn.execute(
        "INSERT INTO workspaces (id, name, slug) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        &[&workspace_id, &"My Workspace".to_string(), &ws_slug],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "insert workspace");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    txn.execute(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
         VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING",
        &[&workspace_id, &user_id],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "insert workspace member");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Consume the invite code atomically with user creation.
    if let Some(code) = invite_code {
        txn.execute(
            "UPDATE invite_codes SET used_by = $1, used_at = now() WHERE code = $2",
            &[&user_id, &code],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "consume invite code");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    txn.commit().await.map_err(|e| {
        tracing::error!(error = %e, "commit txn");
        StatusCode::INTERNAL_SERVER_ERROR
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
) -> Result<Json<CreateInvitesResponse>, StatusCode> {
    let secret = headers
        .get("x-internal-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if secret != state.internal_secret {
        return Err(StatusCode::FORBIDDEN);
    }

    let count = req.count.unwrap_or(1).clamp(1, 50) as usize;
    let db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut codes = Vec::with_capacity(count);
    for _ in 0..count {
        let code = generate_invite_code();
        db.execute(
            "INSERT INTO invite_codes (code) VALUES ($1)",
            &[&code],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "insert invite code");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        codes.push(code);
    }

    tracing::info!(count, "invite codes generated");
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
fn extract_caller(headers: &HeaderMap) -> Result<Uuid, StatusCode> {
    let raw = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
        .ok_or(StatusCode::UNAUTHORIZED)?;
    raw.parse().map_err(|_| StatusCode::UNAUTHORIZED)
}

// ── GET /users/me ─────────────────────────────────────────────────────────────

pub async fn get_me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let caller = extract_caller(&headers)?;
    let db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = db
        .query_opt(
            "SELECT id, email, name FROM users WHERE id = $1 AND deleted_at IS NULL",
            &[&caller],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

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
) -> Result<Json<serde_json::Value>, StatusCode> {
    let caller = extract_caller(&headers)?;
    let db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
) -> Result<Json<serde_json::Value>, StatusCode> {
    let caller = extract_caller(&headers)?;
    let wid: Uuid = params.workspace_id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
) -> Result<Json<serde_json::Value>, StatusCode> {
    let caller = extract_caller(&headers)?;

    if body.name.is_empty() || body.slug.is_empty() || body.workspace_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let wid: Uuid = body.workspace_id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let member = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
            &[&wid, &caller],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !member.get::<_, bool>(0) {
        return Err(StatusCode::FORBIDDEN);
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
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
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
) -> Result<Json<serde_json::Value>, StatusCode> {
    let caller = extract_caller(&headers)?;
    let db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
) -> Result<Json<serde_json::Value>, StatusCode> {
    let caller = extract_caller(&headers)?;

    if body.deploy_id.is_empty() || body.project_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let pid: Uuid = body.project_id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = db
        .query_opt(
            "SELECT workspace_id FROM projects WHERE id = $1 AND deleted_at IS NULL",
            &[&pid],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let wid: Uuid = row.get("workspace_id");

    let member = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
            &[&wid, &caller],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !member.get::<_, bool>(0) {
        return Err(StatusCode::FORBIDDEN);
    }

    let (engine_prefix, artifact_name) = match body.engine.as_str() {
        "liquid" => ("wasm", "main.wasm"),
        "metal"  => ("metal", "app"),
        _        => return Err(StatusCode::BAD_REQUEST),
    };

    let artifact_key = format!(
        "{}/{}/{}/{}",
        engine_prefix, body.project_id, body.deploy_id, artifact_name
    );

    let expires = aws_sdk_s3::presigning::PresigningConfig::expires_in(
        std::time::Duration::from_secs(300),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let presigned = state
        .s3
        .put_object()
        .bucket(&state.bucket)
        .key(&artifact_key)
        .presigned(expires)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "S3 presign failed");
            StatusCode::INTERNAL_SERVER_ERROR
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
) -> Result<Json<serde_json::Value>, StatusCode> {
    let caller = extract_caller(&headers)?;

    if body.name.is_empty() || body.name.len() > 63 || body.artifact_key.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let pid: Uuid = body.project_id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = db
        .query_opt(
            "SELECT workspace_id FROM projects WHERE id = $1 AND deleted_at IS NULL",
            &[&pid],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let wid: Uuid = row.get("workspace_id");

    let member = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
            &[&wid, &caller],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !member.get::<_, bool>(0) {
        return Err(StatusCode::FORBIDDEN);
    }

    let tier_row = db
        .query_one("SELECT tier FROM workspaces WHERE id = $1", &[&wid])
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let tier: String = tier_row.get("tier");
    let limits = crate::quota::limits_for(&tier);

    let engine_str = match body.engine.as_str() {
        "liquid" => "liquid",
        "metal"  => "metal",
        _        => return Err(StatusCode::BAD_REQUEST),
    };

    let engine_spec = match engine_str {
        "metal" => {
            let vcpu      = body.vcpu.unwrap_or(0);
            let memory_mb = body.memory_mb.unwrap_or(0);
            let port      = body.port.unwrap_or(0) as u16;
            if vcpu == 0 || memory_mb < 64 || port == 0 {
                return Err(StatusCode::BAD_REQUEST);
            }
            if vcpu > limits.max_vcpu {
                return Err(StatusCode::UNPROCESSABLE_ENTITY);
            }
            if memory_mb > limits.max_memory_mb {
                return Err(StatusCode::UNPROCESSABLE_ENTITY);
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
        _ => common::EngineSpec::Liquid(common::LiquidSpec {
            artifact_key:    body.artifact_key.clone(),
            artifact_sha256: if body.sha256.is_empty() { None } else { Some(body.sha256.clone()) },
        }),
    };

    let (vcpu_i, memory_mb_i, port_i) = match &engine_spec {
        common::EngineSpec::Metal(m) => (m.vcpu as i32, m.memory_mb as i32, m.port as i32),
        common::EngineSpec::Liquid(_) => (0i32, 0i32, 0i32),
    };

    let service_id = Uuid::now_v7();
    let slug = if body.slug.is_empty() { common::slugify(&body.name) } else { body.slug.clone() };

    // Advisory lock: serializes concurrent deploys for the same workspace.
    let lock_key = {
        let b = wid.as_bytes();
        i64::from_be_bytes(b[0..8].try_into().unwrap())
            ^ i64::from_be_bytes(b[8..16].try_into().unwrap())
    };

    let txn = db
        .build_transaction()
        .start()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    txn.execute("SELECT pg_advisory_xact_lock($1)", &[&lock_key])
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let svc_count: i64 = txn
        .query_one(
            "SELECT COUNT(*) FROM services WHERE workspace_id = $1 AND deleted_at IS NULL",
            &[&wid],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .get(0);

    if svc_count >= limits.max_services {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

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
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    txn.commit().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let event = common::ProvisionEvent {
        tenant_id:  wid.to_string(),
        service_id: service_id.to_string(),
        app_name:   body.name.clone(),
        engine:     if engine_str == "metal" { common::Engine::Metal } else { common::Engine::Liquid },
        spec:       engine_spec,
    };

    crate::nats::publish_provision(&state.nats, &event)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "nats publish failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

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
) -> Result<Json<serde_json::Value>, StatusCode> {
    let caller = extract_caller(&headers)?;
    let service_id: Uuid = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let limit = params.limit.unwrap_or(100).clamp(1, 1000);

    let db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let ok = db
        .query_one(
            "SELECT EXISTS(\
               SELECT 1 FROM services s \
               JOIN projects p ON p.id = s.project_id \
               JOIN workspace_members wm \
                 ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
               WHERE s.id = $1 AND s.deleted_at IS NULL\
             )",
            &[&service_id, &caller],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !ok.get::<_, bool>(0) {
        return Err(StatusCode::NOT_FOUND);
    }

    let rows = db
        .query(
            "SELECT content \
             FROM build_log_lines \
             WHERE service_id = $1 \
               AND created_at > NOW() - INTERVAL '30 days' \
             ORDER BY created_at DESC \
             LIMIT $2",
            &[&service_id, &limit],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let lines: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| serde_json::json!({
            "ts":      serde_json::Value::Null,
            "message": row.get::<_, String>("content"),
        }))
        .collect();

    Ok(Json(serde_json::Value::Array(lines)))
}

// ── POST /services/:id/stop ───────────────────────────────────────────────────

pub async fn stop_service(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let caller = extract_caller(&headers)?;
    let service_id: Uuid = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
             RETURNING s.engine",
            &[&service_id, &caller],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = row.ok_or(StatusCode::NOT_FOUND)?;
    let engine_str: String = row.get("engine");

    let event = common::events::DeprovisionEvent {
        service_id: service_id.to_string(),
        engine: if engine_str == "metal" { common::Engine::Metal } else { common::Engine::Liquid },
    };
    crate::nats::publish_deprovision(&state.nats, &event)
        .await
        .map_err(|e| tracing::error!(error = %e, "nats deprovision publish failed"))
        .ok();

    Ok(StatusCode::NO_CONTENT)
}

// ── POST /services/:id/restart ────────────────────────────────────────────────

pub async fn restart_service(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let caller = extract_caller(&headers)?;
    let service_id: Uuid = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let db = state.db.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let engine_str: String = row.get("engine");
    let wid: Uuid           = row.get("workspace_id");
    let name: String        = row.get("name");
    let slug: String        = row.get("slug");
    let artifact_key: String = row.get::<_, Option<String>>("artifact_key").unwrap_or_default();

    let spec = match engine_str.as_str() {
        "metal" => common::EngineSpec::Metal(common::MetalSpec {
            vcpu:            row.get::<_, i32>("vcpu") as u32,
            memory_mb:       row.get::<_, i32>("memory_mb") as u32,
            port:            row.get::<_, i32>("port") as u16,
            artifact_key:    artifact_key,
            artifact_sha256: None,
            quota:           Default::default(),
        }),
        _ => common::EngineSpec::Liquid(common::LiquidSpec {
            artifact_key:    artifact_key,
            artifact_sha256: None,
        }),
    };

    db.execute(
        "UPDATE services SET status = 'provisioning', upstream_addr = NULL WHERE id = $1",
        &[&service_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let event = common::ProvisionEvent {
        tenant_id:  wid.to_string(),
        service_id: service_id.to_string(),
        app_name:   name.clone(),
        engine:     if engine_str == "metal" { common::Engine::Metal } else { common::Engine::Liquid },
        spec,
    };

    crate::nats::publish_provision(&state.nats, &event)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "nats provision publish failed");
            StatusCode::INTERNAL_SERVER_ERROR
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
