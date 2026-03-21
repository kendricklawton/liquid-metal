use std::sync::Arc;

use axum::{Json, extract::State, http::HeaderMap};
use uuid::Uuid;

use crate::AppState;
use common::contract::{self, ProvisionRequest, ProvisionResponse, CreateInvitesRequest, CreateInvitesResponse};
use super::{ApiError, db_conn, verify_internal_secret};

// ── GET /healthz ─────────────────────────────────────────────────────────────

#[utoipa::path(get, path = "/healthz", responses(
    (status = 200, description = "All subsystems healthy", body = contract::HealthResponse),
    (status = 503, description = "One or more subsystems degraded", body = contract::HealthResponse),
), tag = "Health")]
/// GET /healthz — probes DB and NATS, returns 200 only when both are reachable.
pub async fn health(State(state): State<Arc<AppState>>) -> (axum::http::StatusCode, Json<contract::HealthResponse>) {
    use std::time::Duration;

    let db_ok = match state.db.get().await {
        Ok(conn) => conn.query_one("SELECT 1", &[]).await.is_ok(),
        Err(_)   => false,
    };

    let nats_ok = tokio::time::timeout(
        Duration::from_secs(2),
        state.nats_client.flush(),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);

    let status = if db_ok && nats_ok { axum::http::StatusCode::OK } else { axum::http::StatusCode::SERVICE_UNAVAILABLE };

    (status, Json(contract::HealthResponse {
        status: if db_ok && nats_ok { "ok" } else { "degraded" }.to_string(),
        db:     if db_ok   { "ok" } else { "error" }.to_string(),
        nats:   if nats_ok { "ok" } else { "error" }.to_string(),
    }))
}

// ── GET /auth/cli/config ────────────────────────────────────────────────────

#[utoipa::path(get, path = "/auth/cli/config", responses(
    (status = 200, description = "OIDC device flow configuration"),
), tag = "Auth")]
/// GET /auth/cli/config — returns the OIDC endpoints and client_id for device flow.
pub async fn cli_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut cfg = serde_json::json!({
        "client_id":       state.oidc_cli_client_id,
        "device_auth_url": state.oidc_device_auth_url,
        "token_url":       state.oidc_token_url,
        "userinfo_url":    state.oidc_userinfo_url,
    });
    if let Some(url) = &state.oidc_revoke_url {
        cfg["revoke_url"] = serde_json::Value::String(url.clone());
    }
    Json(cfg)
}

// ── POST /auth/provision ─────────────────────────────────────────────────────

#[utoipa::path(post, path = "/auth/provision", request_body = ProvisionRequest, responses(
    (status = 200, description = "User provisioned", body = ProvisionResponse),
    (status = 403, description = "Invalid internal secret"),
), tag = "Auth", security(("internal_secret" = [])))]
/// POST /auth/provision — upsert user + workspace on first browser login.
/// Protected by X-Internal-Secret header.
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
        None,
    ).await;

    if let Ok(ref r) = resp {
        tracing::info!(target: "audit", action = "provision_user", user_id = r.id, email = req.email, result = "ok");
    }
    resp
}

// ── POST /auth/cli/provision ─────────────────────────────────────────────────

#[utoipa::path(post, path = "/auth/cli/provision", request_body = ProvisionRequest, responses(
    (status = 200, description = "User provisioned via CLI device flow", body = ProvisionResponse),
    (status = 403, description = "Invalid or missing invite code"),
), tag = "Auth")]
/// POST /auth/cli/provision — provision a user after device flow login.
/// New users must supply a valid invite_code.
pub async fn cli_provision(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ProvisionRequest>,
) -> Result<Json<ProvisionResponse>, ApiError> {
    if req.email.is_empty() {
        return Err(ApiError::bad_request("missing_email", "email is required"));
    }
    let mut db = db_conn(&state.db).await?;
    let Json(mut resp) = do_provision(
        &mut db,
        &state.features,
        &req.email,
        &req.first_name,
        &req.last_name,
        req.oidc_sub.as_deref(),
        req.invite_code.as_deref(),
    ).await?;

    // Auto-create an admin-scoped API key for the CLI session.
    let token = super::api_keys::generate_token();
    let token_hash = super::hash_token(&token);
    let token_prefix = format!("{}...", &token[..12]);
    let key_id = Uuid::now_v7();
    let user_id: Uuid = resp.id.parse().map_err(|_| ApiError::internal("invalid user id"))?;

    db.execute(
        "INSERT INTO api_keys (id, user_id, name, token_hash, token_prefix, scopes) \
         VALUES ($1, $2, 'cli-login', $3, $4, $5)",
        &[&key_id, &user_id, &token_hash, &token_prefix, &vec!["admin".to_string()]],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "failed to create CLI API key");
        ApiError::internal("failed to create API key")
    })?;

    resp.api_key = Some(token);

    tracing::info!(target: "audit", action = "cli_provision", user_id = %user_id, email = req.email, result = "ok");
    Ok(Json(resp))
}

// ── Shared provisioning logic ────────────────────────────────────────────────

async fn do_provision(
    db:          &mut deadpool_postgres::Object,
    features:    &common::Features,
    email:       &str,
    first_name:  &str,
    last_name:   &str,
    oidc_sub: Option<&str>,
    invite_code: Option<&str>,
) -> Result<Json<ProvisionResponse>, ApiError> {
    if let Some(sub) = oidc_sub {
        db.execute(
            "UPDATE users SET oidc_sub = $1 WHERE email = $2 AND deleted_at IS NULL",
            &[&sub, &email],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "update oidc_sub");
            ApiError::internal("failed to update OIDC subject")
        })?;
    }

    let existing = db
        .query_opt(
            "SELECT u.id, u.name, u.oidc_sub, w.id AS workspace_id, w.slug \
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
            tier:         String::new(),
            workspace_id: row.get::<_, Option<Uuid>>("workspace_id")
                              .map(|u| u.to_string())
                              .unwrap_or_default(),
            oidc_sub:  row.get("oidc_sub"),
            api_key: None,
        }));
    }

    // New user — validate invite code.
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

    let proposed_user_id = Uuid::now_v7();
    let full_name = format!("{} {}", first_name, last_name).trim().to_string();
    let full_name = if full_name.is_empty() { email.to_string() } else { full_name };

    let txn = db.transaction().await.map_err(|e| {
        tracing::error!(error = %e, "begin txn");
        ApiError::internal("failed to start transaction")
    })?;

    // Upsert user — RETURNING gives us the actual ID whether inserted or existing.
    let row = txn.query_one(
        "INSERT INTO users (id, email, name, oidc_sub) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (email) DO UPDATE SET \
           oidc_sub = COALESCE(EXCLUDED.oidc_sub, users.oidc_sub), \
           name = EXCLUDED.name \
         RETURNING id",
        &[&proposed_user_id, &email, &full_name, &oidc_sub],
    ).await.map_err(|e| {
        tracing::error!(error = ?e, "upsert user");
        ApiError::internal("failed to create user")
    })?;
    let user_id: Uuid = row.get("id");

    // Use the full user_id (without hyphens) as workspace slug suffix.
    // UUID v7 shares the first 8 chars (timestamp) between users created
    // close together, so we need the full ID to guarantee uniqueness.
    let uid_hex = user_id.to_string().replace('-', "");
    let ws_slug = format!("{}-{}", workspace_slug(&full_name), uid_hex);
    let workspace_id = Uuid::now_v7();

    txn.execute(
        "INSERT INTO workspaces (id, name, slug) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        &[&workspace_id, &"My Workspace".to_string(), &ws_slug],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "insert workspace");
        ApiError::internal("failed to create workspace")
    })?;

    // Use the workspace that was actually inserted (may differ if slug existed
    // from a previous provision of the same user).
    let ws_row = txn.query_one(
        "SELECT id, slug FROM workspaces WHERE slug = $1 AND deleted_at IS NULL",
        &[&ws_slug],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "lookup workspace");
        ApiError::internal("failed to find workspace")
    })?;
    let workspace_id: Uuid = ws_row.get("id");
    let ws_slug: String = ws_row.get("slug");

    txn.execute(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
         VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING",
        &[&workspace_id, &user_id],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "insert workspace member");
        ApiError::internal("failed to create workspace membership")
    })?;

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
        tier:         String::new(),
        workspace_id: workspace_id.to_string(),
        oidc_sub:  oidc_sub.map(|s| s.to_string()),
        api_key:  None,
    }))
}

// ── Admin: invite code generation ───────────────────────────────────────────

#[utoipa::path(post, path = "/admin/invites", request_body = CreateInvitesRequest, responses(
    (status = 200, description = "Invite codes generated", body = CreateInvitesResponse),
    (status = 403, description = "Invalid internal secret"),
), tag = "Admin", security(("internal_secret" = [])))]
/// POST /admin/invites — generate single-use invite codes.
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

fn generate_invite_code() -> String {
    let id = Uuid::new_v4().to_string().replace('-', "");
    let upper = id[..8].to_uppercase();
    format!("{}-{}", &upper[..4], &upper[4..8])
}

fn workspace_slug(name: &str) -> String {
    format!("{}-workspace", common::slugify(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::slugify;

    #[test]
    fn slugify_lowercase() { assert_eq!(slugify("MyApp"), "myapp"); }
    #[test]
    fn slugify_spaces_become_dashes() { assert_eq!(slugify("my app"), "my-app"); }
    #[test]
    fn slugify_collapses_consecutive_dashes() { assert_eq!(slugify("my--app"), "my-app"); }
    #[test]
    fn slugify_trims_leading_trailing_dashes() { assert_eq!(slugify("  my app  "), "my-app"); }
    #[test]
    fn slugify_special_chars() { assert_eq!(slugify("hello/world!"), "hello-world"); }
    #[test]
    fn slugify_empty() { assert_eq!(slugify(""), ""); }
    #[test]
    fn workspace_slug_appends_suffix() { assert_eq!(workspace_slug("Alice Smith"), "alice-smith-workspace"); }
    #[test]
    fn workspace_slug_collapses_dashes() {
        assert_eq!(workspace_slug("Alice  Smith"), "alice-smith-workspace");
        assert_eq!(workspace_slug("Alice   Smith"), "alice-smith-workspace");
    }
    #[test]
    fn workspace_slug_email_fallback() {
        let s = workspace_slug("alice@example.com");
        assert!(s.ends_with("-workspace"));
    }
}
