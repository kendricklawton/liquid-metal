use std::sync::Arc;

use axum::{Extension, Json, extract::{Path, State}};
use uuid::Uuid;

use crate::AppState;
use crate::envelope;
use common::contract;
use super::{ApiError, Caller, db_conn, require_scope};

/// Look up project workspace_id and verify caller is a member.
async fn resolve_project(
    db: &deadpool_postgres::Object,
    project_id: Uuid,
    user_id: Uuid,
) -> Result<Uuid, ApiError> {
    let row = db
        .query_opt(
            "SELECT p.workspace_id \
             FROM projects p \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE p.id = $1 AND p.deleted_at IS NULL",
            &[&project_id, &user_id],
        )
        .await
        .map_err(|_| ApiError::internal("project lookup failed"))?
        .ok_or_else(|| ApiError::not_found("project not found"))?;

    Ok(row.get("workspace_id"))
}

// ── GET /projects/:id/env ────────────────────────────────────────────────────

#[utoipa::path(get, path = "/projects/{id}/env", params(
    ("id" = String, Path, description = "Project UUID"),
), responses(
    (status = 200, description = "Project environment variables", body = contract::EnvVarsResponse),
    (status = 404, description = "Project not found"),
), tag = "Projects", security(("api_key" = [])))]
pub async fn get_project_env_vars(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<contract::EnvVarsResponse>, ApiError> {
    require_scope(&caller, "read")?;
    let project_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_project_id", "project ID must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;
    let workspace_id = resolve_project(&db, project_id, caller.user_id).await?;

    let vars = envelope::read_project_env_vars(&state.vault, workspace_id, project_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %project_id, "failed to read project env vars from vault");
            ApiError::internal("failed to read project environment variables")
        })?;

    Ok(Json(contract::EnvVarsResponse { vars }))
}

// ── POST /projects/:id/env ───────────────────────────────────────────────────

#[utoipa::path(post, path = "/projects/{id}/env", request_body = contract::SetEnvVarsRequest, params(
    ("id" = String, Path, description = "Project UUID"),
), responses(
    (status = 200, description = "Project environment variables updated", body = contract::EnvVarsResponse),
    (status = 404, description = "Project not found"),
), tag = "Projects", security(("api_key" = [])))]
pub async fn set_project_env_vars(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(req): Json<contract::SetEnvVarsRequest>,
) -> Result<Json<contract::EnvVarsResponse>, ApiError> {
    require_scope(&caller, "admin")?;
    let project_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_project_id", "project ID must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;
    let workspace_id = resolve_project(&db, project_id, caller.user_id).await?;

    // Read existing, merge new values (new values win).
    let mut vars = envelope::read_project_env_vars(&state.vault, workspace_id, project_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %project_id, "failed to read project env vars");
            ApiError::internal("failed to read project environment variables")
        })?;

    vars.extend(req.vars);

    envelope::store_project_env_vars(&state.vault, workspace_id, project_id, &vars)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %project_id, "failed to store project env vars");
            ApiError::internal("failed to store project environment variables")
        })?;

    tracing::info!(target: "audit", action = "set_project_env_vars", user_id = %caller.user_id, %project_id);
    Ok(Json(contract::EnvVarsResponse { vars }))
}

// ── POST /projects/:id/env/unset ─────────────────────────────────────────────

#[utoipa::path(post, path = "/projects/{id}/env/unset", request_body = contract::UnsetEnvVarsRequest, params(
    ("id" = String, Path, description = "Project UUID"),
), responses(
    (status = 200, description = "Keys removed from project env vars", body = contract::EnvVarsResponse),
    (status = 404, description = "Project not found"),
), tag = "Projects", security(("api_key" = [])))]
pub async fn unset_project_env_vars(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(req): Json<contract::UnsetEnvVarsRequest>,
) -> Result<Json<contract::EnvVarsResponse>, ApiError> {
    require_scope(&caller, "admin")?;
    let project_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_project_id", "project ID must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;
    let workspace_id = resolve_project(&db, project_id, caller.user_id).await?;

    let mut vars = envelope::read_project_env_vars(&state.vault, workspace_id, project_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %project_id, "failed to read project env vars");
            ApiError::internal("failed to read project environment variables")
        })?;

    for key in &req.keys {
        vars.remove(key);
    }

    envelope::store_project_env_vars(&state.vault, workspace_id, project_id, &vars)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %project_id, "failed to store project env vars");
            ApiError::internal("failed to store project environment variables")
        })?;

    tracing::info!(target: "audit", action = "unset_project_env_vars", user_id = %caller.user_id, %project_id);
    Ok(Json(contract::EnvVarsResponse { vars }))
}
