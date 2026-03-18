use std::sync::Arc;

use axum::{Extension, Json, extract::{Path, State}};
use uuid::Uuid;

use crate::AppState;
use common::contract;
use super::{ApiError, Caller, db_conn, require_scope};

#[utoipa::path(post, path = "/services/{id}/scale", request_body = contract::ScaleRequest, params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "Run mode updated", body = contract::ScaleResponse),
    (status = 404, description = "Service not found"),
    (status = 422, description = "Tier does not support always-on"),
), tag = "Services", security(("api_key" = [])))]
pub async fn scale_service(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<contract::ScaleRequest>,
) -> Result<Json<contract::ScaleResponse>, ApiError> {
    require_scope(&caller, "write")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;

    let mode = body.mode.to_lowercase();
    if mode != "serverless" && mode != "always-on" {
        return Err(ApiError::bad_request("invalid_mode", "mode must be 'serverless' or 'always-on'"));
    }

    let db = db_conn(&state.db).await?;

    // Check service exists and get workspace tier.
    let svc = db
        .query_opt(
            "SELECT s.slug, w.tier FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspaces w ON w.id = p.workspace_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let slug: String = svc.get("slug");
    let tier: String = svc.get("tier");

    if mode == "always-on" {
        let limits = crate::quota::limits_for(&tier);
        if !limits.allows_always_on {
            return Err(ApiError::unprocessable("tier_limit", format!("always-on requires Pro or Team plan (current: {})", tier)));
        }
    }

    db.execute(
        "UPDATE services SET run_mode = $2 WHERE id = $1 AND deleted_at IS NULL",
        &[&service_id, &mode],
    )
    .await
    .map_err(|_| ApiError::internal("failed to update run mode"))?;

    Ok(Json(contract::ScaleResponse {
        id: service_id.to_string(),
        slug,
        run_mode: mode,
    }))
}
