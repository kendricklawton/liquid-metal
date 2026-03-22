use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
};
use uuid::Uuid;

use super::{ApiError, db_conn, hash_token, require_scope};
use crate::AppState;
use common::contract;

const VALID_SCOPES: &[&str] = &["read", "write", "admin"];

/// Generate a cryptographically random `lm_` prefixed API token.
/// Uses two UUIDv4s concatenated for 256 bits of entropy, hex-encoded.
pub(crate) fn generate_token() -> String {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    format!("lm_{}{}", a.simple(), b.simple())
}

#[utoipa::path(get, path = "/api-keys", responses(
    (status = 200, description = "List of API keys", body = contract::ApiKeyListResponse),
), tag = "API Keys", security(("api_key" = [])))]
pub async fn list_api_keys(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<super::Caller>,
) -> Result<Json<contract::ApiKeyListResponse>, ApiError> {
    require_scope(&caller, "read")?;

    let db = db_conn(&state.db).await?;
    let rows = db
        .query(
            "SELECT id, name, token_prefix, scopes, expires_at, last_used_at, created_at \
             FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL \
             ORDER BY created_at DESC",
            &[&caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("failed to list API keys"))?;

    let keys = rows
        .iter()
        .map(|row| {
            let expires_at: Option<chrono::DateTime<chrono::Utc>> = row.get("expires_at");
            let last_used_at: Option<chrono::DateTime<chrono::Utc>> = row.get("last_used_at");
            let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");
            contract::ApiKeyResponse {
                id: row.get::<_, Uuid>("id").to_string(),
                name: row.get("name"),
                token_prefix: row.get("token_prefix"),
                scopes: row.get("scopes"),
                expires_at: expires_at.map(|t| t.to_rfc3339()),
                last_used_at: last_used_at.map(|t| t.to_rfc3339()),
                created_at: created_at.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(contract::ApiKeyListResponse { keys }))
}

#[utoipa::path(post, path = "/api-keys", request_body = contract::CreateApiKeyRequest, responses(
    (status = 201, description = "API key created", body = contract::CreateApiKeyResponse),
), tag = "API Keys", security(("api_key" = [])))]
pub async fn create_api_key(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<super::Caller>,
    Json(body): Json<contract::CreateApiKeyRequest>,
) -> Result<(axum::http::StatusCode, Json<contract::CreateApiKeyResponse>), ApiError> {
    require_scope(&caller, "admin")?;

    if body.name.is_empty() || body.name.len() > 64 {
        return Err(ApiError::bad_request(
            "invalid_name",
            "name must be 1-64 characters",
        ));
    }

    let scopes: Vec<String> = if body.scopes.is_empty() {
        vec!["read".to_string()]
    } else {
        for s in &body.scopes {
            if !VALID_SCOPES.contains(&s.as_str()) {
                return Err(ApiError::bad_request(
                    "invalid_scope",
                    format!("invalid scope '{}' — must be one of: read, write, admin", s),
                ));
            }
        }
        body.scopes
    };

    let token = generate_token();
    let token_hash = hash_token(&token);
    let token_prefix = format!("{}...", &token[..12]);

    let expires_at = body
        .expires_in_days
        .map(|days| chrono::Utc::now() + chrono::Duration::days(days as i64));

    let db = db_conn(&state.db).await?;
    let row = db
        .query_one(
            "INSERT INTO api_keys (user_id, name, token_hash, token_prefix, scopes, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id",
            &[
                &caller.user_id,
                &body.name,
                &token_hash,
                &token_prefix,
                &scopes,
                &expires_at,
            ],
        )
        .await
        .map_err(|_| ApiError::internal("failed to create API key"))?;

    let key_id: Uuid = row.get("id");

    tracing::info!(target: "audit", action = "create_api_key", user_id = %caller.user_id, ip = ?caller.ip, key_id = %key_id, scopes = ?scopes);

    Ok((
        axum::http::StatusCode::CREATED,
        Json(contract::CreateApiKeyResponse {
            id: key_id.to_string(),
            name: body.name,
            token,
            scopes,
            expires_at: expires_at.map(|t| t.to_rfc3339()),
        }),
    ))
}

#[utoipa::path(delete, path = "/api-keys/{id}", params(
    ("id" = String, Path, description = "API key UUID"),
), responses(
    (status = 200, description = "API key deleted", body = contract::DeleteApiKeyResponse),
    (status = 404, description = "API key not found"),
), tag = "API Keys", security(("api_key" = [])))]
pub async fn delete_api_key(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<super::Caller>,
    Path(id): Path<String>,
) -> Result<Json<contract::DeleteApiKeyResponse>, ApiError> {
    require_scope(&caller, "admin")?;

    let key_id: Uuid = id
        .parse()
        .map_err(|_| ApiError::bad_request("invalid_id", "API key ID must be a valid UUID"))?;

    let db = db_conn(&state.db).await?;
    let rows_affected = db
        .execute(
            "UPDATE api_keys SET deleted_at = NOW() \
             WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL",
            &[&key_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("failed to delete API key"))?;

    if rows_affected == 0 {
        return Err(ApiError::not_found("API key not found"));
    }

    tracing::info!(target: "audit", action = "delete_api_key", user_id = %caller.user_id, ip = ?caller.ip, key_id = %key_id);

    Ok(Json(contract::DeleteApiKeyResponse {
        id: key_id.to_string(),
        deleted: true,
    }))
}
