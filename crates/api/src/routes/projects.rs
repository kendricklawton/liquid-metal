use std::sync::Arc;

use axum::{Extension, Json, extract::{Query, State}};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use common::contract;
use super::{ApiError, Caller, db_conn, require_scope, require_workspace_role};

#[derive(Deserialize)]
pub struct ListProjectsParams {
    workspace_id: String,
}

#[utoipa::path(get, path = "/projects", params(
    ("workspace_id" = String, Query, description = "Workspace UUID to list projects for"),
), responses(
    (status = 200, description = "List of projects in the workspace", body = Vec<contract::ProjectResponse>),
), tag = "Projects", security(("api_key" = [])))]
pub async fn list_projects(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Query(params): Query<ListProjectsParams>,
) -> Result<Json<Vec<contract::ProjectResponse>>, ApiError> {
    require_scope(&caller, "read")?;
    let wid: Uuid = params.workspace_id.parse().map_err(|_| ApiError::bad_request("invalid_workspace_id", "workspace_id must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;

    let rows = db
        .query(
            "SELECT p.id, p.workspace_id, p.name, p.slug \
             FROM projects p \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE p.workspace_id = $1 AND p.deleted_at IS NULL \
             ORDER BY p.created_at DESC \
             LIMIT 200",
            &[&wid, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("failed to list projects"))?;

    let projects: Vec<contract::ProjectResponse> = rows
        .iter()
        .map(|row| contract::ProjectResponse {
            id: row.get::<_, Uuid>("id").to_string(),
            workspace_id: row.get::<_, Uuid>("workspace_id").to_string(),
            name: row.get("name"),
            slug: row.get("slug"),
        })
        .collect();

    Ok(Json(projects))
}

#[utoipa::path(post, path = "/projects", request_body = contract::CreateProjectRequest, responses(
    (status = 200, description = "Project created", body = contract::CreateProjectResponse),
    (status = 409, description = "Project slug already exists"),
), tag = "Projects", security(("api_key" = [])))]
pub async fn create_project(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<contract::CreateProjectRequest>,
) -> Result<Json<contract::CreateProjectResponse>, ApiError> {
    require_scope(&caller, "write")?;

    if body.name.is_empty() || body.slug.is_empty() || body.workspace_id.is_empty() {
        return Err(ApiError::bad_request("missing_fields", "name, slug, and workspace_id are required"));
    }
    if !body.slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(ApiError::bad_request("invalid_slug", "slug must contain only lowercase alphanumeric characters and dashes"));
    }

    let wid: Uuid = body.workspace_id.parse().map_err(|_| ApiError::bad_request("invalid_workspace_id", "workspace_id must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;

    let member = db
        .query_opt(
            "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
            &[&wid, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("workspace membership check failed"))?
        .ok_or_else(|| ApiError::forbidden("not a member of this workspace"))?;
    let role: String = member.get("role");
    require_workspace_role(&role, "admin")?;

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
            ApiError::conflict("a project with this slug already exists in this workspace")
        } else {
            ApiError::internal("failed to create project")
        }
    })?;

    tracing::info!(
        target: "audit",
        action = "create_project",
        user_id = %caller.user_id,
        ip = ?caller.ip,
        project_id = %project_id,
        workspace_id = %wid,
        slug = body.slug,
    );

    Ok(Json(contract::CreateProjectResponse {
        project: contract::ProjectResponse {
            id: project_id.to_string(),
            workspace_id: wid.to_string(),
            name: body.name,
            slug: body.slug,
        },
    }))
}
