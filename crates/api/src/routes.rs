use crate::AppState;
use axum::{
    Json,
    extract::{Request, State},
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

/// GET /auth/cli/config — returns the Zitadel domain and client_id for device flow.
///
/// Neither value is a secret (both are public OAuth identifiers), so this
/// endpoint requires no authentication. The CLI fetches them at login time so
/// no credentials need to be baked into the binary.
pub async fn cli_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "zitadel_domain":    state.zitadel_domain,
        "zitadel_client_id": state.zitadel_client_id,
    }))
}

// ── Auth: shared types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ProvisionRequest {
    pub email:       String,
    pub first_name:  String,
    pub last_name:   String,
    pub zitadel_sub: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProvisionResponse {
    pub id:           String,
    pub name:         String,
    pub slug:         String,
    pub tier:         String,
    pub workspace_id: String,
    pub zitadel_sub:  Option<String>,
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
        req.zitadel_sub.as_deref(),
        None, // web BFF path — no invite required (internal-secret gated)
    ).await
}

// ── Auth: CLI provision (PKCE callback) ──────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CliProvisionRequest {
    pub email:       String,
    pub first_name:  String,
    pub last_name:   String,
    pub zitadel_sub: Option<String>,
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
        req.zitadel_sub.as_deref(),
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
    zitadel_sub: Option<&str>,
    invite_code: Option<&str>,
) -> Result<Json<ProvisionResponse>, StatusCode> {
    // Upsert zitadel_sub on the existing user if provided.
    if let Some(sub) = zitadel_sub {
        db.execute(
            "UPDATE users SET zitadel_sub = $1 WHERE email = $2 AND deleted_at IS NULL",
            &[&sub, &email],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "update zitadel_sub");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    // Fast path: return existing user (no invite check — they already provisioned).
    let existing = db
        .query_opt(
            "SELECT u.id, u.name, u.tier, u.zitadel_sub, w.id AS workspace_id, w.slug \
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
            zitadel_sub:  row.get("zitadel_sub"),
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
        "INSERT INTO users (id, email, name, zitadel_sub) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (email) DO UPDATE SET zitadel_sub = EXCLUDED.zitadel_sub \
         WHERE EXCLUDED.zitadel_sub IS NOT NULL",
        &[&user_id, &email, &full_name, &zitadel_sub],
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
        zitadel_sub:  zitadel_sub.map(|s| s.to_string()),
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
