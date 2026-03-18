use std::sync::Arc;

use axum::{Extension, Json, extract::{Path, Query, State}};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use common::contract;
use super::{ApiError, Caller, db_conn, require_scope};

#[derive(Deserialize)]
pub struct LogsParams {
    limit: Option<i64>,
}

#[utoipa::path(get, path = "/services/{id}/logs", params(
    ("id" = String, Path, description = "Service UUID"),
    ("limit" = Option<i64>, Query, description = "Max log lines (default 100, max 1000)"),
), responses(
    (status = 200, description = "Service log lines", body = Vec<contract::LogLineResponse>),
    (status = 404, description = "Service not found"),
), tag = "Services", security(("api_key" = [])))]
pub async fn get_service_logs(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(params): Query<LogsParams>,
) -> Result<Json<Vec<contract::LogLineResponse>>, ApiError> {
    require_scope(&caller, "read")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;
    let limit = params.limit.unwrap_or(100).clamp(1, 1000);

    let db = db_conn(&state.db).await?;

    // Verify caller has access and fetch the service slug for VictoriaLogs query.
    let row = db
        .query_opt(
            "SELECT s.slug FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?;

    let slug: String = match row {
        Some(r) => r.get("slug"),
        None => return Err(ApiError::not_found("service not found")),
    };

    // Query VictoriaLogs for this service's logs (collected by Promtail).
    let vlogs_url = &state.victorialogs_url;
    if vlogs_url.is_empty() {
        return Ok(Json(vec![]));
    }

    // Sanitize slug for LogQL injection: only allow alphanumeric + dashes.
    // Slugs *should* already be in this format, but defense-in-depth against
    // any slug that was stored before validation was tightened.
    let safe_slug: String = slug.chars().filter(|c| c.is_alphanumeric() || *c == '-').collect();
    if safe_slug.is_empty() || safe_slug != slug {
        tracing::warn!(%slug, %safe_slug, "slug contained unexpected characters for log query");
    }
    let query = format!("task:\"{}\"", safe_slug);
    let resp = state.http_client
        .get(format!("{vlogs_url}/select/logsql/query"))
        .query(&[("query", &query), ("limit", &limit.to_string())])
        .send()
        .await
        .map_err(|_| ApiError::bad_gateway("log backend unreachable"))?;

    if !resp.status().is_success() {
        tracing::warn!(status = %resp.status(), slug, "VictoriaLogs query failed");
        return Err(ApiError::bad_gateway("log query failed"));
    }

    // VictoriaLogs returns newline-delimited JSON. Each line is a log entry.
    let body = resp.text().await.map_err(|_| ApiError::bad_gateway("failed to read log response"))?;
    let lines: Vec<contract::LogLineResponse> = body
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let obj: serde_json::Value = serde_json::from_str(l).ok()?;
            Some(contract::LogLineResponse {
                ts: obj.get("_time").and_then(|v| v.as_str()).map(|s| s.to_string()),
                message: obj.get("_msg")
                    .or_else(|| obj.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect();

    Ok(Json(lines))
}
