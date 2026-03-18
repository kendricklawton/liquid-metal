use std::sync::Arc;

use axum::{Extension, Json, extract::{Path, State}};
use uuid::Uuid;

use crate::AppState;
use crate::envelope;
use common::contract;
use super::{ApiError, Caller, db_conn, require_scope};

// ── GET /services/:id/env ─────────────────────────────────────────────────────

#[utoipa::path(get, path = "/services/{id}/env", params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "Environment variables", body = contract::EnvVarsResponse),
    (status = 404, description = "Service not found"),
), tag = "Services", security(("api_key" = [])))]
pub async fn get_env_vars(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<contract::EnvVarsResponse>, ApiError> {
    require_scope(&caller, "read")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;

    let db = db_conn(&state.db).await?;

    let row = db
        .query_opt(
            "SELECT s.env_ciphertext, s.env_nonce, p.workspace_id \
             FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("env vars lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let ciphertext: Option<Vec<u8>> = row.get("env_ciphertext");
    let nonce: Option<Vec<u8>> = row.get("env_nonce");

    let vars = match (ciphertext, nonce) {
        (Some(ct), Some(n)) => {
            let workspace_id: Uuid = row.get("workspace_id");
            envelope::decrypt_env_vars(&db, &*state.kms, workspace_id, &ct, &n)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, %service_id, "env var decryption failed");
                    ApiError::internal("failed to decrypt environment variables")
                })?
        }
        _ => std::collections::HashMap::new(),
    };

    Ok(Json(contract::EnvVarsResponse { vars }))
}

// ── POST /services/:id/env ───────────────────────────────────────────────────

#[utoipa::path(post, path = "/services/{id}/env", request_body = contract::SetEnvVarsRequest, params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "Environment variables updated", body = contract::EnvVarsResponse),
    (status = 404, description = "Service not found"),
), tag = "Services", security(("api_key" = [])))]
pub async fn set_env_vars(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<contract::SetEnvVarsRequest>,
) -> Result<Json<contract::EnvVarsResponse>, ApiError> {
    require_scope(&caller, "write")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;

    let db = db_conn(&state.db).await?;

    // Fetch existing encrypted env vars + workspace_id.
    let row = db
        .query_opt(
            "SELECT s.env_ciphertext, s.env_nonce, p.workspace_id \
             FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("env vars lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let workspace_id: Uuid = row.get("workspace_id");
    let ciphertext: Option<Vec<u8>> = row.get("env_ciphertext");
    let nonce: Option<Vec<u8>> = row.get("env_nonce");

    // Decrypt existing vars (if any), merge new ones in.
    let mut vars = match (ciphertext, nonce) {
        (Some(ct), Some(n)) => {
            envelope::decrypt_env_vars(&db, &*state.kms, workspace_id, &ct, &n)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, %service_id, "env var decryption failed during set");
                    ApiError::internal("failed to decrypt environment variables")
                })?
        }
        _ => std::collections::HashMap::new(),
    };

    vars.extend(body.vars.into_iter());

    // Re-encrypt the merged map and store.
    let (new_ct, new_nonce) = envelope::encrypt_env_vars(&db, &*state.kms, workspace_id, &vars)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %service_id, "env var encryption failed");
            ApiError::internal("failed to encrypt environment variables")
        })?;

    db.execute(
        "UPDATE services SET env_ciphertext = $2, env_nonce = $3 \
         WHERE id = $1",
        &[&service_id, &new_ct, &new_nonce],
    )
    .await
    .map_err(|_| ApiError::internal("failed to update env vars"))?;

    Ok(Json(contract::EnvVarsResponse { vars }))
}

// ── POST /services/:id/env/unset ─────────────────────────────────────────────

#[utoipa::path(post, path = "/services/{id}/env/unset", request_body = contract::UnsetEnvVarsRequest, params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "Keys removed", body = contract::EnvVarsResponse),
    (status = 404, description = "Service not found"),
), tag = "Services", security(("api_key" = [])))]
pub async fn unset_env_vars(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<contract::UnsetEnvVarsRequest>,
) -> Result<Json<contract::EnvVarsResponse>, ApiError> {
    require_scope(&caller, "write")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;

    let db = db_conn(&state.db).await?;

    // Fetch existing encrypted env vars + workspace_id.
    let row = db
        .query_opt(
            "SELECT s.env_ciphertext, s.env_nonce, p.workspace_id \
             FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("env vars lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let workspace_id: Uuid = row.get("workspace_id");
    let ciphertext: Option<Vec<u8>> = row.get("env_ciphertext");
    let nonce: Option<Vec<u8>> = row.get("env_nonce");

    // Decrypt, remove requested keys, re-encrypt.
    let mut vars = match (ciphertext, nonce) {
        (Some(ct), Some(n)) => {
            envelope::decrypt_env_vars(&db, &*state.kms, workspace_id, &ct, &n)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, %service_id, "env var decryption failed during unset");
                    ApiError::internal("failed to decrypt environment variables")
                })?
        }
        _ => std::collections::HashMap::new(),
    };

    for key in &body.keys {
        vars.remove(key);
    }

    // Re-encrypt and store.
    let (new_ct, new_nonce) = envelope::encrypt_env_vars(&db, &*state.kms, workspace_id, &vars)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %service_id, "env var encryption failed");
            ApiError::internal("failed to encrypt environment variables")
        })?;

    db.execute(
        "UPDATE services SET env_ciphertext = $2, env_nonce = $3 \
         WHERE id = $1",
        &[&service_id, &new_ct, &new_nonce],
    )
    .await
    .map_err(|_| ApiError::internal("failed to update env vars"))?;

    Ok(Json(contract::EnvVarsResponse { vars }))
}
