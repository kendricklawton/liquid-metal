use std::sync::Arc;

use axum::{Extension, Json, extract::{Path, State}, http::StatusCode};
use uuid::Uuid;

use crate::AppState;
use common::contract;
use super::{ApiError, Caller, db_conn, require_scope};

#[utoipa::path(get, path = "/workspaces", responses(
    (status = 200, description = "List of workspaces the caller belongs to", body = Vec<contract::WorkspaceResponse>),
), tag = "Workspaces", security(("api_key" = [])))]
pub async fn list_workspaces(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<Vec<contract::WorkspaceResponse>>, ApiError> {
    require_scope(&caller, "read")?;
    let db = db_conn(&state.db).await?;

    let rows = db
        .query(
            "SELECT w.id, w.name, w.slug \
             FROM workspaces w \
             JOIN workspace_members wm ON wm.workspace_id = w.id AND wm.user_id = $1 \
             WHERE w.deleted_at IS NULL \
             ORDER BY w.created_at ASC \
             LIMIT 50",
            &[&caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("failed to list workspaces"))?;

    let workspaces: Vec<contract::WorkspaceResponse> = rows
        .iter()
        .map(|row| contract::WorkspaceResponse {
            id: row.get::<_, Uuid>("id").to_string(),
            name: row.get("name"),
            slug: row.get("slug"),
            tier: String::new(),
        })
        .collect();

    Ok(Json(workspaces))
}

#[utoipa::path(delete, path = "/workspaces/{id}", params(
    ("id" = String, Path, description = "Workspace UUID"),
), responses(
    (status = 204, description = "Workspace deleted"),
    (status = 404, description = "Workspace not found or not an owner"),
), tag = "Workspaces", security(("api_key" = [])))]
/// Soft-delete a workspace and deprovision all its running services.
pub async fn delete_workspace(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_scope(&caller, "admin")?;
    let wid: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_workspace_id", "workspace ID must be a valid UUID"))?;

    let db = db_conn(&state.db).await?;

    let owns = db
        .query_opt(
            "SELECT 1 FROM workspace_members \
             WHERE workspace_id = $1 AND user_id = $2 AND role = 'owner'",
            &[&wid, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("ownership check failed"))?;

    if owns.is_none() {
        return Err(ApiError::not_found("workspace not found"));
    }

    let running = db
        .query(
            "SELECT id::text, slug, engine FROM services \
             WHERE workspace_id = $1 AND deleted_at IS NULL \
               AND status IN ('running', 'provisioning')",
            &[&wid],
        )
        .await
        .map_err(|_| ApiError::internal("failed to list running services"))?;

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

    db.execute(
        "UPDATE services SET status = 'stopped', upstream_addr = NULL, deleted_at = NOW() \
         WHERE workspace_id = $1 AND deleted_at IS NULL",
        &[&wid],
    ).await.map_err(|_| ApiError::internal("failed to delete services"))?;

    db.execute(
        "UPDATE projects SET deleted_at = NOW() WHERE workspace_id = $1 AND deleted_at IS NULL",
        &[&wid],
    ).await.map_err(|_| ApiError::internal("failed to delete projects"))?;

    db.execute(
        "UPDATE workspaces SET deleted_at = NOW() WHERE id = $1 AND deleted_at IS NULL",
        &[&wid],
    ).await.map_err(|_| ApiError::internal("failed to delete workspace"))?;

    tracing::info!(
        target: "audit",
        action = "delete_workspace",
        user_id = %caller.user_id,
        workspace_id = %wid,
        services_deprovisioned = running.len(),
        result = "ok",
    );

    Ok(StatusCode::NO_CONTENT)
}
