//! API route handlers, split by domain.
//!
//! Shared infrastructure (ApiError, db_conn, auth_middleware) lives here.
//! Each submodule owns the handlers for its domain.

mod acme;
mod api_keys;
mod auth;
mod deployments;
mod domains;
mod env;
mod logs;
mod projects;
mod scale;
mod services;
mod users;
mod workspaces;

// Re-export everything so lib.rs paths stay unchanged (routes::health, etc.)
pub use acme::*;
pub use api_keys::*;
pub use auth::*;
pub use deployments::*;
pub use domains::*;
pub use env::*;
pub use logs::*;
pub use projects::*;
pub use services::*;
pub use users::*;
pub use workspaces::*;

use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::AppState;

/// Maximum time to wait for a DB connection from the pool before returning 503.
static DB_POOL_TIMEOUT: std::sync::LazyLock<Duration> = std::sync::LazyLock::new(|| {
    let secs: u64 = common::config::env_or("DB_POOL_TIMEOUT_SECS", "5")
        .parse()
        .unwrap_or(5);
    Duration::from_secs(secs)
});

// ── Structured API error ─────────────────────────────────────────────────────

/// Structured error response returned by all API handlers.
///
/// Serializes to `{"error": "...", "message": "..."}` so clients can
/// programmatically distinguish error types and show human-readable context.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub(crate) status: StatusCode,
    error: &'static str,
    message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, error: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            error,
            message: message.into(),
        }
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
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            message,
        )
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

/// Verify a raw secret value against the allowed list using constant-time comparison.
/// Accepts any of the provided secrets — supports zero-downtime rotation.
pub(crate) fn verify_internal_secret_value(
    secret: &str,
    valid_secrets: &[String],
) -> Result<(), ApiError> {
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

/// Verify the X-Internal-Secret header using constant-time comparison.
/// Accepts any of the provided secrets — supports zero-downtime rotation.
pub(crate) fn verify_internal_secret(
    headers: &HeaderMap,
    valid_secrets: &[String],
) -> Result<(), ApiError> {
    let secret = headers
        .get("x-internal-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    verify_internal_secret_value(secret, valid_secrets)
}

/// Acquire a DB connection with a timeout. Returns 503 Service Unavailable
/// if the pool is exhausted.
pub(crate) async fn db_conn(
    pool: &deadpool_postgres::Pool,
) -> Result<deadpool_postgres::Object, ApiError> {
    match tokio::time::timeout(*DB_POOL_TIMEOUT, pool.get()).await {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "db pool error");
            Err(ApiError::internal("database connection failed"))
        }
        Err(_) => {
            tracing::warn!("db pool timeout — all connections busy");
            Err(ApiError::unavailable(
                "database connection pool exhausted — try again",
            ))
        }
    }
}

/// Extract the raw token string from X-Api-Key or Authorization: Bearer.
fn extract_raw_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
        .ok_or_else(|| ApiError::unauthorized("missing X-Api-Key or Authorization header"))
}

/// Resolved caller identity from the auth middleware.
/// Stored in request extensions so handlers can access it.
#[derive(Debug, Clone)]
pub struct Caller {
    pub user_id: Uuid,
    /// Scopes from the API key, or ["admin"] for legacy UUID tokens.
    pub scopes: Vec<String>,
    /// API key ID if authenticated via `lm_` token, for audit logging.
    pub api_key_id: Option<Uuid>,
}

/// Check that the caller has the required scope.
/// Scope hierarchy: admin > write > read.
pub(crate) fn require_scope(caller: &Caller, required: &str) -> Result<(), ApiError> {
    let has = |s: &str| caller.scopes.iter().any(|sc| sc == s);
    let allowed = match required {
        "read" => has("read") || has("write") || has("admin"),
        "write" => has("write") || has("admin"),
        "admin" => has("admin"),
        _ => false,
    };
    if !allowed {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "insufficient_scope",
            format!("this action requires the '{}' scope", required),
        ));
    }
    Ok(())
}

/// Hash an `lm_` token to its SHA-256 hex digest for DB lookup.
pub(crate) fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ── Auth middleware ───────────────────────────────────────────────────────────

/// Per-user auth middleware: validates the caller from either:
/// - Internal service: `X-Internal-Secret` + `X-On-Behalf-Of: {user_id}` (web BFF)
/// - Scoped API key: `X-Api-Key: lm_...` or `Authorization: Bearer lm_...`
///
/// On success, inserts a `Caller` into request extensions for downstream handlers.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let caller = if let Some(secret) = req.headers().get("x-internal-secret") {
        // ── Internal service path (web BFF) ──
        let secret_str = secret
            .to_str()
            .map_err(|_| ApiError::unauthorized("invalid X-Internal-Secret header"))?;
        verify_internal_secret_value(secret_str, &state.internal_secrets)?;

        let on_behalf_of = req
            .headers()
            .get("x-on-behalf-of")
            .ok_or_else(|| {
                ApiError::bad_request(
                    "missing_header",
                    "X-On-Behalf-Of required with X-Internal-Secret on protected routes",
                )
            })?
            .to_str()
            .map_err(|_| ApiError::bad_request("invalid_header", "invalid X-On-Behalf-Of value"))?;

        let user_id: Uuid = on_behalf_of
            .parse()
            .map_err(|_| ApiError::bad_request("invalid_user_id", "X-On-Behalf-Of must be a valid UUID"))?;

        let db = db_conn(&state.db).await?;
        let exists: bool = db
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at IS NULL)",
                &[&user_id],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "user lookup in internal auth");
                ApiError::internal("authentication check failed")
            })?
            .get(0);

        if !exists {
            return Err(ApiError::unauthorized("unknown or deleted user"));
        }

        Caller {
            user_id,
            scopes: vec!["admin".to_string()],
            api_key_id: None,
        }
    } else {
        // ── Scoped API key path ──
        let raw = extract_raw_token(req.headers())?;

        if !raw.starts_with("lm_") {
            return Err(ApiError::unauthorized(
                "invalid API key format \u{2014} expected lm_* scoped token (run: flux login)",
            ));
        }

        let token_hash = hash_token(raw);
        let db = db_conn(&state.db).await?;

        let row = db
            .query_opt(
                "SELECT ak.id, ak.user_id, ak.scopes, ak.expires_at \
                 FROM api_keys ak \
                 JOIN users u ON u.id = ak.user_id AND u.deleted_at IS NULL \
                 WHERE ak.token_hash = $1 AND ak.deleted_at IS NULL",
                &[&token_hash],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "api_key lookup failed");
                ApiError::internal("authentication check failed")
            })?
            .ok_or_else(|| ApiError::unauthorized("invalid API key"))?;

        let key_id: Uuid = row.get("id");
        let user_id: Uuid = row.get("user_id");
        let scopes: Vec<String> = row.get("scopes");
        let expires_at: Option<chrono::DateTime<chrono::Utc>> = row.get("expires_at");

        if let Some(exp) = expires_at {
            if exp < chrono::Utc::now() {
                return Err(ApiError::unauthorized("API key has expired"));
            }
        }

        // Update last_used_at (fire-and-forget, don't block the request)
        let pool = state.db.clone();
        tokio::spawn(async move {
            let fut = async {
                let db = pool.get().await.ok()?;
                db.execute(
                    "UPDATE api_keys SET last_used_at = NOW() WHERE id = $1",
                    &[&key_id],
                ).await.ok()
            };
            if tokio::time::timeout(std::time::Duration::from_secs(5), fut).await.is_err() {
                tracing::warn!(%key_id, "last_used_at update timed out");
            }
        });

        Caller {
            user_id,
            scopes,
            api_key_id: Some(key_id),
        }
    };

    req.extensions_mut().insert(caller);
    Ok(next.run(req).await)
}
