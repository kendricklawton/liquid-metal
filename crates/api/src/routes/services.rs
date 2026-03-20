use std::sync::Arc;

use axum::{Json, Extension, extract::{Path, State}, http::StatusCode};
use uuid::Uuid;

use crate::AppState;
use crate::envelope;
use common::contract;
use super::{ApiError, Caller, db_conn, require_scope};

#[utoipa::path(get, path = "/services", responses(
    (status = 200, description = "List of services the caller has access to", body = Vec<contract::ServiceResponse>),
), tag = "Services", security(("api_key" = [])))]
pub async fn list_services(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<Vec<contract::ServiceResponse>>, ApiError> {
    require_scope(&caller, "read")?;
    let db = db_conn(&state.db).await?;

    let rows = db
        .query(
            "SELECT s.id, s.name, s.slug, s.engine, s.status, s.upstream_addr, s.failure_reason, s.created_at::text \
             FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $1 \
             WHERE s.deleted_at IS NULL \
             ORDER BY s.created_at DESC \
             LIMIT 500",
            &[&caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("failed to list services"))?;

    let services: Vec<contract::ServiceResponse> = rows
        .iter()
        .map(|row| contract::ServiceResponse {
            id: row.get::<_, Uuid>("id").to_string(),
            name: row.get("name"),
            slug: row.get("slug"),
            engine: row.get("engine"),
            status: row.get("status"),
            upstream_addr: row.get("upstream_addr"),
            failure_reason: row.get("failure_reason"),
            created_at: row.get("created_at"),
        })
        .collect();

    Ok(Json(services))
}

#[utoipa::path(post, path = "/services/{id}/stop", params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 204, description = "Service stopped"),
    (status = 404, description = "Service not found"),
    (status = 409, description = "Service already stopped"),
), tag = "Services", security(("api_key" = [])))]
pub async fn stop_service(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_scope(&caller, "write")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;

    let mut db = db_conn(&state.db).await?;

    let existing = db
        .query_opt(
            "SELECT s.status FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let current_status: String = existing.get("status");
    if current_status == "stopped" {
        return Err(ApiError::conflict("service is already stopped"));
    }

    let txn = db
        .build_transaction()
        .start()
        .await
        .map_err(|_| ApiError::internal("failed to start transaction"))?;

    let row = txn
        .query_opt(
            "UPDATE services s \
             SET status = 'stopped', upstream_addr = NULL \
             FROM projects p, workspace_members wm \
             WHERE s.id = $1 \
               AND s.deleted_at IS NULL \
               AND s.status != 'stopped' \
               AND s.project_id = p.id \
               AND wm.workspace_id = p.workspace_id \
               AND wm.user_id = $2 \
             RETURNING s.engine, s.slug",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("failed to stop service"))?
        .ok_or_else(|| ApiError::conflict("service state changed concurrently"))?;

    let engine_str: String = row.get("engine");
    let slug: String       = row.get("slug");
    let engine: common::Engine = engine_str.parse().map_err(|_| {
        tracing::error!(engine = engine_str, service_id = %service_id, "unknown engine in DB");
        ApiError::internal("corrupt engine value in database")
    })?;

    // Remove any pending provision events for this service.
    if let Err(e) = txn.execute(
        "DELETE FROM outbox WHERE payload->>'service_id' = $1",
        &[&service_id.to_string()],
    ).await {
        tracing::warn!(error = %e, %service_id, "failed to clean outbox during stop");
    }

    let event = common::events::DeprovisionEvent {
        service_id: service_id.to_string(),
        slug,
        engine,
    };
    let payload = serde_json::to_value(&event).map_err(|e| {
        tracing::error!(error = %e, "serializing deprovision event");
        ApiError::internal("failed to serialize deprovision event")
    })?;
    txn.execute(
        "INSERT INTO outbox (subject, payload) VALUES ($1, $2)",
        &[&common::events::SUBJECT_DEPROVISION, &payload],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "outbox insert failed");
        ApiError::internal("failed to queue deprovision event")
    })?;

    txn.commit().await.map_err(|_| ApiError::internal("failed to commit stop transaction"))?;

    tracing::info!(target: "audit", action = "stop_service", user_id = %caller.user_id, service_id = %service_id, slug = event.slug);

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/services/{id}/delete", params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "Service deleted", body = contract::DeleteServiceResponse),
    (status = 404, description = "Service not found"),
), tag = "Services", security(("api_key" = [])))]
pub async fn delete_service(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<contract::DeleteServiceResponse>, ApiError> {
    require_scope(&caller, "admin")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;

    let mut db = db_conn(&state.db).await?;

    let existing = db
        .query_opt(
            "SELECT s.status, s.slug, s.engine, s.artifact_key FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let current_status: String = existing.get("status");
    let slug: String = existing.get("slug");
    let engine_str: String = existing.get("engine");
    let artifact_key: Option<String> = existing.get("artifact_key");

    let needs_deprovision = current_status != "stopped" && current_status != "failed";

    let txn = db
        .build_transaction()
        .start()
        .await
        .map_err(|_| ApiError::internal("failed to start transaction"))?;

    txn.execute(
        "UPDATE services SET status = 'stopped', upstream_addr = NULL, deleted_at = NOW() \
         WHERE id = $1 AND deleted_at IS NULL",
        &[&service_id],
    ).await.map_err(|_| ApiError::internal("failed to delete service"))?;

    // Remove any pending provision events for this service.
    if let Err(e) = txn.execute(
        "DELETE FROM outbox WHERE payload->>'service_id' = $1",
        &[&service_id.to_string()],
    ).await {
        tracing::warn!(error = %e, %service_id, "failed to clean outbox during delete");
    }

    if needs_deprovision {
        if let Ok(engine) = engine_str.parse::<common::Engine>() {
            let event = common::events::DeprovisionEvent {
                service_id: service_id.to_string(),
                slug: slug.clone(),
                engine,
            };
            let payload = serde_json::to_value(&event).map_err(|e| {
                tracing::error!(error = %e, "serializing deprovision event");
                ApiError::internal("failed to serialize deprovision event")
            })?;
            txn.execute(
                "INSERT INTO outbox (subject, payload) VALUES ($1, $2)",
                &[&common::events::SUBJECT_DEPROVISION, &payload],
            ).await.map_err(|e| {
                tracing::error!(error = %e, "outbox insert failed");
                ApiError::internal("failed to queue deprovision event")
            })?;
        }
    }

    txn.commit().await.map_err(|_| ApiError::internal("failed to commit delete transaction"))?;

    if let Some(key) = artifact_key {
        let s3 = state.s3.clone();
        let bucket = state.bucket.clone();
        tokio::spawn(async move {
            if let Err(e) = s3.delete_object().bucket(&bucket).key(&key).send().await {
                tracing::warn!(error = %e, key, "failed to delete artifact from S3");
            }
        });
    }

    tracing::info!(target: "audit", action = "delete_service", user_id = %caller.user_id, service_id = %service_id, slug);

    Ok(Json(contract::DeleteServiceResponse {
        id: service_id.to_string(),
        slug,
        deleted: true,
    }))
}

#[utoipa::path(post, path = "/services/{id}/restart", params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "Service restarting", body = contract::DeployResponse),
    (status = 404, description = "Service not found"),
    (status = 409, description = "Service not in stopped/failed state"),
), tag = "Services", security(("api_key" = [])))]
pub async fn restart_service(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<contract::DeployResponse>, ApiError> {
    require_scope(&caller, "write")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;

    let mut db = db_conn(&state.db).await?;

    let existing = db
        .query_opt(
            "SELECT s.status FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let current_status: String = existing.get("status");
    if current_status != "stopped" && current_status != "failed" {
        return Err(ApiError::conflict(format!("service is currently '{}' — only stopped or failed services can be restarted", current_status)));
    }

    let row = db
        .query_opt(
            "SELECT s.id, s.name, s.slug, s.engine, s.workspace_id, \
                    s.vcpu, s.memory_mb, s.port, s.artifact_key, s.commit_sha, \
                    s.env_ciphertext, s.env_nonce \
             FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm \
               ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 \
               AND s.deleted_at IS NULL \
               AND s.status IN ('stopped', 'failed')",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::conflict("service state changed concurrently"))?;

    let engine_str: String = row.get("engine");
    let engine: common::Engine = engine_str.parse().map_err(|_| {
        tracing::error!(engine = engine_str, service_id = %service_id, "unknown engine in DB");
        ApiError::internal("corrupt engine value in database")
    })?;
    let wid: Uuid           = row.get("workspace_id");
    let name: String        = row.get("name");
    let slug: String        = row.get("slug");
    let artifact_key: String = row.get::<_, Option<String>>("artifact_key").unwrap_or_default();

    let spec = match engine {
        common::Engine::Metal => common::EngineSpec::Metal(common::MetalSpec {
            vcpu:            row.get::<_, i32>("vcpu") as u32,
            memory_mb:       row.get::<_, i32>("memory_mb") as u32,
            port:            row.get::<_, i32>("port") as u16,
            artifact_key:    artifact_key,
            artifact_sha256: None,
            quota:           state.default_quota.clone(),
        }),
        common::Engine::Liquid => common::EngineSpec::Liquid(common::LiquidSpec {
            artifact_key:    artifact_key,
            artifact_sha256: None,
        }),
    };

    // Decrypt env vars from the encrypted columns (not the legacy plaintext column).
    let ciphertext: Option<Vec<u8>> = row.get("env_ciphertext");
    let nonce: Option<Vec<u8>> = row.get("env_nonce");
    let env_vars = match (ciphertext, nonce) {
        (Some(ct), Some(n)) => {
            envelope::decrypt_env_vars(&db, &*state.kms, wid, &ct, &n)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, %service_id, "env var decryption failed");
                    ApiError::internal("failed to decrypt environment variables")
                })?
        }
        _ => std::collections::HashMap::new(),
    };

    let event = common::ProvisionEvent {
        tenant_id:  wid.to_string(),
        service_id: service_id.to_string(),
        app_name:   name.clone(),
        slug:       slug.clone(),
        engine:     engine,
        spec,
        env_vars,
    };

    // Use the outbox pattern: status update + outbox insert in one transaction.
    let payload = serde_json::to_value(&event).map_err(|e| {
        tracing::error!(error = %e, "serializing provision event");
        ApiError::internal("failed to serialize provision event")
    })?;

    let txn = db
        .build_transaction()
        .start()
        .await
        .map_err(|_| ApiError::internal("failed to start transaction"))?;

    txn.execute(
        "UPDATE services SET status = 'provisioning', upstream_addr = NULL WHERE id = $1",
        &[&service_id],
    ).await.map_err(|_| ApiError::internal("failed to update service status"))?;

    txn.execute(
        "INSERT INTO outbox (subject, payload) VALUES ($1, $2)",
        &[&common::events::SUBJECT_PROVISION, &payload],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "outbox insert failed");
        ApiError::internal("failed to queue restart event")
    })?;

    txn.commit().await.map_err(|_| ApiError::internal("failed to commit restart transaction"))?;

    tracing::info!(target: "audit", action = "restart_service", user_id = %caller.user_id, service_id = %service_id, slug, engine = engine_str);

    Ok(Json(contract::DeployResponse {
        service: contract::DeployedService {
            id: service_id.to_string(),
            name,
            slug,
            status: "provisioning".to_string(),
        },
    }))
}
