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
            "SELECT p.workspace_id \
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

    let vars = envelope::read_env_vars(&state.vault, workspace_id, service_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %service_id, "failed to read env vars from vault");
            ApiError::internal("failed to read environment variables")
        })?;

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

    let row = db
        .query_opt(
            "SELECT p.workspace_id \
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

    // Read existing vars from Vault, merge new ones in.
    let mut vars = envelope::read_env_vars(&state.vault, workspace_id, service_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %service_id, "failed to read env vars from vault during set");
            ApiError::internal("failed to read environment variables")
        })?;

    vars.extend(body.vars.into_iter());

    // Write merged map back to Vault.
    envelope::store_env_vars(&state.vault, workspace_id, service_id, &vars)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %service_id, "failed to write env vars to vault");
            ApiError::internal("failed to store environment variables")
        })?;

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

    let row = db
        .query_opt(
            "SELECT p.workspace_id \
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

    // Read existing vars from Vault, remove requested keys, write back.
    let mut vars = envelope::read_env_vars(&state.vault, workspace_id, service_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %service_id, "failed to read env vars from vault during unset");
            ApiError::internal("failed to read environment variables")
        })?;

    for key in &body.keys {
        vars.remove(key);
    }

    envelope::store_env_vars(&state.vault, workspace_id, service_id, &vars)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %service_id, "failed to write env vars to vault");
            ApiError::internal("failed to store environment variables")
        })?;

    Ok(Json(contract::EnvVarsResponse { vars }))
}
