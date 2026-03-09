use crate::{AppState, nats};
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
    routing::{delete, get, post},
};
use common::events::{
    DeprovisionEvent, Engine, EngineSpec, LiquidSpec, MetalSpec, ProvisionEvent, ResourceQuota,
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
    if std::env::var("DISABLE_AUTH").as_deref() == Ok("1") {
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

// ── Upload URL ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UploadUrlQuery {
    pub engine: String,  // "metal" | "liquid"
    pub name:   String,  // service name (for key path)
}

#[derive(Debug, Serialize)]
pub struct UploadUrlResponse {
    pub upload_url:   String,
    pub artifact_key: String,
    pub deploy_id:    String,
    pub expires_in:   u64,  // seconds
}

/// GET /upload-url?engine=metal&name=my-app
///
/// Returns a pre-signed PUT URL pointing directly at Vultr Object Storage.
/// The CLI PUTs the artifact to this URL without routing it through the API,
/// then calls POST /services with the artifact_key.
pub async fn upload_url(
    State(state): State<Arc<AppState>>,
    Query(q): Query<UploadUrlQuery>,
) -> Result<Json<UploadUrlResponse>, StatusCode> {
    let deploy_id = Uuid::now_v7().to_string();

    let ext = match q.engine.as_str() {
        "metal"  => "rootfs.ext4",
        "liquid" => "main.wasm",
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let artifact_key = format!("{}/{}/{}/{}", q.engine, q.name, deploy_id, ext);
    let expires_secs = 600u64; // 10 minutes

    let presigned = state
        .s3
        .put_object()
        .bucket(&state.bucket)
        .key(&artifact_key)
        .presigned(
            aws_sdk_s3::presigning::PresigningConfig::expires_in(
                Duration::from_secs(expires_secs),
            )
            .map_err(|e| {
                tracing::error!(error = %e, "presigning config error");
                StatusCode::INTERNAL_SERVER_ERROR
            })?,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "presigned PUT URL generation failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(UploadUrlResponse {
        upload_url: presigned.uri().to_string(),
        artifact_key,
        deploy_id,
        expires_in: expires_secs,
    }))
}

// ── Services CRUD ─────────────────────────────────────────────────────────────

pub fn services_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/",    post(create_service))
        .route("/{id}", get(get_service))
        .route("/{id}", delete(delete_service))
}

#[derive(Debug, Deserialize)]
pub struct CreateServiceRequest {
    pub workspace_id:  Uuid,
    pub project_id:    Option<Uuid>,
    pub name:          String,
    pub engine:        String,   // "metal" | "liquid"
    pub artifact_key:  String,   // Object Storage key from /upload-url
    pub artifact_sha256: Option<String>,
    // Git context (populated by flux deploy)
    pub branch:         Option<String>,
    pub commit_sha:     Option<String>,
    pub commit_message: Option<String>,
    // Metal config
    pub port:      Option<u16>,
    pub vcpu:      Option<u32>,
    pub memory_mb: Option<u32>,
    // Resource quotas (Triple-Lock layers 2 + 3)
    #[serde(default)]
    pub quota: ResourceQuota,
}

#[derive(Debug, Serialize)]
pub struct ServiceResponse {
    pub id:            String,
    pub name:          String,
    pub slug:          String,
    pub engine:        String,
    pub status:        String,
    pub upstream_addr: Option<String>,
}

pub async fn create_service(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateServiceRequest>,
) -> Result<(StatusCode, Json<ServiceResponse>), StatusCode> {
    let service_id = Uuid::now_v7();
    let slug       = slugify(&req.name);
    let port       = req.port.unwrap_or(8080);
    let vcpu       = req.vcpu.unwrap_or(1);
    let memory_mb  = req.memory_mb.unwrap_or(128);

    let spec = match req.engine.as_str() {
        "metal" => EngineSpec::Metal(MetalSpec {
            vcpu, memory_mb, port,
            artifact_key:    req.artifact_key.clone(),
            artifact_sha256: req.artifact_sha256.clone(),
            quota:           req.quota.clone(),
        }),
        "liquid" => EngineSpec::Liquid(LiquidSpec {
            artifact_key:    req.artifact_key.clone(),
            artifact_sha256: req.artifact_sha256.clone(),
        }),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let engine = match req.engine.as_str() {
        "metal"  => Engine::Metal,
        "liquid" => Engine::Liquid,
        _        => return Err(StatusCode::BAD_REQUEST),
    };

    let db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool error");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    db.execute(
        "INSERT INTO services \
         (id, workspace_id, project_id, name, slug, engine, \
          branch, commit_sha, commit_message, \
          vcpu, memory_mb, port, rootfs_path, wasm_path, \
          disk_read_bps, disk_write_bps, disk_read_iops, disk_write_iops, \
          net_ingress_kbps, net_egress_kbps) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)",
        &[
            &service_id,
            &req.workspace_id,
            &req.project_id,
            &req.name,
            &slug,
            &req.engine,
            &req.branch,
            &req.commit_sha,
            &req.commit_message,
            &(vcpu as i32),
            &(memory_mb as i32),
            &(port as i32),
            &req.artifact_key,   // stored in rootfs_path for metal
            &req.artifact_key,   // stored in wasm_path for liquid
            &req.quota.disk_read_bps.map(|v| v as i64),
            &req.quota.disk_write_bps.map(|v| v as i64),
            &req.quota.disk_read_iops.map(|v| v as i32),
            &req.quota.disk_write_iops.map(|v| v as i32),
            &req.quota.net_ingress_kbps.map(|v| v as i32),
            &req.quota.net_egress_kbps.map(|v| v as i32),
        ],
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert service failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let event = ProvisionEvent {
        tenant_id:  req.workspace_id.to_string(),
        service_id: service_id.to_string(),
        app_name:   req.name.clone(),
        engine,
        spec,
    };

    nats::publish_provision(&state.nats, &event)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to publish provision event");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok((
        StatusCode::ACCEPTED,
        Json(ServiceResponse {
            id:           service_id.to_string(),
            name:         req.name,
            slug,
            engine:       req.engine,
            status:       "provisioning".to_string(),
            upstream_addr: None,
        }),
    ))
}

pub async fn get_service(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ServiceResponse>, StatusCode> {
    let svc_id: Uuid = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool error");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let row = db
        .query_opt(
            "SELECT id, name, slug, engine, status, upstream_addr \
             FROM services WHERE id = $1 AND deleted_at IS NULL",
            &[&svc_id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "query service failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ServiceResponse {
        id:           row.get::<_, Uuid>("id").to_string(),
        name:         row.get("name"),
        slug:         row.get("slug"),
        engine:       row.get("engine"),
        status:       row.get("status"),
        upstream_addr: row.get("upstream_addr"),
    }))
}

pub async fn delete_service(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let svc_id: Uuid = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool error");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let row = db
        .query_opt(
            "UPDATE services SET deleted_at = NOW() \
             WHERE id = $1 AND deleted_at IS NULL \
             RETURNING engine",
            &[&svc_id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "soft-delete service failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let engine_str: String = row.get("engine");
    let engine = match engine_str.as_str() {
        "metal"  => Engine::Metal,
        "liquid" => Engine::Liquid,
        _        => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    // Tell daemon to tear down the VM / Wasm executor
    let event = DeprovisionEvent { service_id: svc_id.to_string(), engine };
    if let Err(e) = nats::publish_deprovision(&state.nats, &event).await {
        tracing::error!(error = %e, service_id = %svc_id, "failed to publish deprovision event");
        // Don't fail the delete — DB row is already soft-deleted
    }

    tracing::info!(service_id = %svc_id, "service deleted");
    Ok(StatusCode::NO_CONTENT)
}

// ── Auth: shared types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ProvisionRequest {
    pub email:      String,
    pub first_name: String,
    pub last_name:  String,
}

#[derive(Debug, Serialize)]
pub struct ProvisionResponse {
    pub id:   String,
    pub name: String,
    pub slug: String,
    pub tier: String,
}

// ── Auth: internal provision (called by Go web BFF) ──────────────────────────

/// POST /auth/provision — upsert user + workspace on first browser login via WorkOS.
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

    do_provision(&mut db, &req.email, &req.first_name, &req.last_name).await
}

// ── Auth: CLI provision (PKCE — no web server required) ──────────────────────

#[derive(Debug, Deserialize)]
pub struct CliProvisionRequest {
    /// User identity fields from the WorkOS PKCE token exchange response.
    pub email:      String,
    pub first_name: String,
    pub last_name:  String,
}

/// POST /auth/cli/provision — provision a user after a successful PKCE flow.
///
/// Called directly by the CLI after it completes the PKCE browser flow.
/// User fields come from the WorkOS token exchange response (which already
/// authenticates the user) — no secondary WorkOS API call needed.
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

    do_provision(&mut db, &req.email, &req.first_name, &req.last_name).await
}

// ── Auth: shared provisioning logic ──────────────────────────────────────────

/// Upsert user + workspace atomically. Returns the user's UUID, display name,
/// workspace slug, and tier. Idempotent — safe to call on every login.
async fn do_provision(
    db:         &mut deadpool_postgres::Object,
    email:      &str,
    first_name: &str,
    last_name:  &str,
) -> Result<Json<ProvisionResponse>, StatusCode> {
    // Fast path: return existing user.
    let existing = db
        .query_opt(
            "SELECT u.id, u.name, u.tier, w.slug \
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
            id:   row.get::<_, Uuid>("id").to_string(),
            name: row.get("name"),
            slug: row.get::<_, Option<String>>("slug").unwrap_or_default(),
            tier: row.get::<_, Option<String>>("tier")
                      .unwrap_or_else(|| "free".to_string()),
        }));
    }

    // New user — create user + workspace atomically.
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
        "INSERT INTO users (id, email, name) VALUES ($1, $2, $3) ON CONFLICT (email) DO NOTHING",
        &[&user_id, &email, &full_name],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "insert user");
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

    txn.commit().await.map_err(|e| {
        tracing::error!(error = %e, "commit txn");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    tracing::info!(email, user_id = %user_id, "user provisioned");

    Ok(Json(ProvisionResponse {
        id:   user_id.to_string(),
        name: full_name,
        slug: ws_slug,
        tier: "free".to_string(),
    }))
}

/// Slugify a display name into a workspace slug.
fn workspace_slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c == '-' {
            if !prev_dash { slug.push(c); }
            prev_dash = true;
        } else {
            slug.push(c);
            prev_dash = false;
        }
    }
    format!("{}-workspace", slug.trim_matches('-'))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Lowercase, replace non-alphanumeric with `-`, collapse runs, trim edges.
fn slugify(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    let mut slug = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c == '-' {
            if !prev_dash { slug.push(c); }
            prev_dash = true;
        } else {
            slug.push(c);
            prev_dash = false;
        }
    }
    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── slugify ───────────────────────────────────────────────────────────────

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
